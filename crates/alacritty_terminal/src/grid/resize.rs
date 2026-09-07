//! Grid resize and reflow.

use std::cmp::{max, min, Ordering};
use std::mem;

use crate::index::{Boundary, Column, Line};
use crate::term::cell::{Flags, ResetDiscriminant};

use crate::grid::row::Row;
use crate::grid::{Dimensions, Grid, GridCell};

impl<T: GridCell + Default + PartialEq> Grid<T> {
    /// Resize the grid's width and/or height.
    pub fn resize<D>(&mut self, reflow: bool, lines: usize, columns: usize)
    where
        T: ResetDiscriminant<D>,
        D: PartialEq,
    {
        self.resize_anchored(reflow, lines, columns, false);
    }

    /// conhost's `TextBuffer::Reflow` (microsoft/terminal textBuffer.cpp),
    /// ported for ConPTY parity — used whenever a ConPTY session's WIDTH
    /// changes. ConPTY emits nothing on a resize and computes every later
    /// absolute cursor position against conhost's own reflow, which differs
    /// from Alacritty's in exactly these measured rules (rikka addition):
    ///
    /// 1. only the viewport takes part, top-down, and only up to
    ///    `max(last row with text, cursor row)` — rows below do not exist
    ///    for the reflow (conhost's `oldHeight` cutoff);
    /// 2. a row contributes its content up to its last non-empty cell —
    ///    trailing blanks never wrap (conhost's `MeasureRight`);
    /// 3. on the cursor's row the contribution extends through the cursor
    ///    column (REFLOW_JANK_CURSOR_WRAP: blanks before the cursor count
    ///    as content, so text typed on a fresh line unwraps onto its
    ///    logical predecessor on a later grow);
    /// 4. a target row filled to the new width wraps (WRAPLINE); a source
    ///    row without WRAPLINE ends its logical line;
    /// 5. the cursor follows its logical position through the copy, and
    ///    content past it may be dropped rather than let the cursor row
    ///    scroll away (conhost's `newYLimit` guard).
    ///
    /// Scrollback is ours alone (ConPTY's conhost has none): history rows
    /// are only width-fitted — they cannot affect conhost's coordinates —
    /// and viewport overflow scrolls INTO history, mirroring conhost's
    /// circular-buffer advance discarding its top rows.
    pub fn resize_conhost<D>(&mut self, lines: usize, columns: usize)
    where
        T: ResetDiscriminant<D> + Clone,
        D: PartialEq,
    {
        let old_cols = self.columns;
        self.columns = columns;

        // Storage is NEWEST-first (index 0 = the viewport's bottom row).
        let mut all = self.raw.take_all();
        let mut viewport: Vec<Row<T>> = all.drain(..self.lines).collect();
        viewport.reverse(); // top row first, for the top-down copy below
        let mut history = all; // newest-first, oldest last

        // History: width-fit only (truncate/pad, no reflow).
        if columns != old_cols {
            for row in &mut history {
                if columns > old_cols {
                    row.grow(columns);
                } else {
                    row.shrink(columns);
                }
            }
        }

        let cursor_y = self.cursor.point.line.0.max(0) as usize;
        let cursor_x = self.cursor.point.column.0;

        // Rule 1: the participation cutoff.
        let last_text = viewport.iter().rposition(|r| !r.is_clear());
        let old_height = max(last_text.map_or(0, |t| t + 1), cursor_y + 1).min(self.lines);

        let mut out: Vec<Row<T>> = Vec::new();
        let mut cur: Row<T> = Row::new(columns);
        let mut new_x = 0usize;
        // (index into `out` at cursor-copy time, column); the row may still
        // be `cur` — its index is out.len() until pushed.
        let mut new_cursor: Option<(usize, usize)> = None;

        for (y, row) in viewport.iter().enumerate().take(old_height) {
            // Rules 2 + 3: the contribution limit.
            let mut limit = (0..old_cols)
                .rev()
                .find(|&x| !row[Column(x)].is_empty())
                .map_or(0, |x| x + 1);
            if y == cursor_y {
                limit = max(limit, min(cursor_x + 1, old_cols));
            }

            let mut old_x = 0usize;
            loop {
                if old_x >= limit {
                    break;
                }
                // Rule 4: forced wrap — only when there is more content to
                // place. Checked AFTER the exhaustion test: a row that fills
                // the new width exactly and then ends its logical line must
                // not wrap first and newline second, or an empty row appears
                // between it and the next line. conhost defers the wrap
                // (measured 2026-09-07: a 44-char line, resize to 44 columns,
                // `[Console]::CursorTop` advanced by exactly the lines typed
                // — no phantom row); the earlier ordering here inserted one.
                if new_x >= columns {
                    // A wide char split by the new boundary moves whole to
                    // the next row, leaving a spacer (same contract as the
                    // native reflow paths).
                    let carry = if cur[Column(columns - 1)].flags().contains(Flags::WIDE_CHAR) {
                        let mut spacer = T::default();
                        spacer.flags_mut().insert(Flags::LEADING_WIDE_CHAR_SPACER);
                        Some(mem::replace(&mut cur[Column(columns - 1)], spacer))
                    } else {
                        None
                    };
                    cur[Column(columns - 1)].flags_mut().insert(Flags::WRAPLINE);
                    out.push(mem::replace(&mut cur, Row::new(columns)));
                    new_x = 0;
                    if let Some(wide) = carry {
                        cur[Column(0)] = wide;
                        new_x = 1;
                    }
                }
                let n = min(limit - old_x, columns - new_x);
                for k in 0..n {
                    cur[Column(new_x + k)] = row[Column(old_x + k)].clone();
                }
                // Rule 5: cursor tracking.
                if y == cursor_y && cursor_x >= old_x && cursor_x < old_x + n {
                    new_cursor = Some((out.len(), new_x + (cursor_x - old_x)));
                }
                old_x += n;
                new_x += n;
            }

            // Rule 4: an explicit newline ends the logical line. WRAPLINE
            // lives on the source row's LAST cell (its old width).
            let wrapped = row
                .last()
                .is_some_and(|c| c.flags().contains(Flags::WRAPLINE));
            if !wrapped {
                out.push(mem::replace(&mut cur, Row::new(columns)));
                new_x = 0;
            }
        }
        if new_x != 0 || new_cursor.is_some_and(|(row, _)| row == out.len()) {
            out.push(cur);
        }

        let (mut cursor_row, cursor_col) = new_cursor.unwrap_or((0, 0));

        // Overflow scrolls into history — but never past the cursor's row
        // (rule 5): content beyond it is dropped instead, like conhost
        // stopping its copy before overwriting the cursor.
        let mut overflow = out.len().saturating_sub(lines);
        if overflow > cursor_row {
            out.truncate(lines + cursor_row);
            overflow = cursor_row;
        }
        if overflow > 0 {
            // Rows scrolled out at the top join the history's NEWEST end.
            let mut pushed: Vec<Row<T>> = out.drain(..overflow).collect();
            pushed.reverse(); // newest-first, like the storage
            pushed.extend(history);
            history = pushed;
            cursor_row -= overflow;
        }
        while out.len() < lines {
            out.push(Row::new(columns));
        }

        self.cursor.point.line = Line(cursor_row as i32);
        self.cursor.point.column = Column(min(cursor_col, columns - 1));
        self.cursor.input_needs_wrap = false;
        self.saved_cursor.point.line = Line(min(self.saved_cursor.point.line.0, lines as i32 - 1));
        self.saved_cursor.point.column = Column(min(self.saved_cursor.point.column.0, columns - 1));

        // Reassemble newest-first: viewport bottom..top, then the history.
        out.reverse();
        out.extend(history);
        out.truncate(self.max_scroll_limit + lines);
        self.raw.replace_inner(out);
        self.raw.set_visible_lines(lines);
        self.lines = lines;
        self.display_offset = min(self.display_offset, self.history_size());
    }

    /// [`Self::resize`] with a choice of growth anchoring: `top_anchored`
    /// growth never pulls rows back from history — blank lines appear at the
    /// bottom and the cursor stays put. This is conhost's behavior; ConPTY
    /// emits nothing on resize and positions all later output with absolute
    /// coordinates computed against exactly that layout, so a ConPTY-backed
    /// terminal must reflow the same way or drift permanently.
    pub fn resize_anchored<D>(
        &mut self,
        reflow: bool,
        lines: usize,
        columns: usize,
        top_anchored: bool,
    ) where
        T: ResetDiscriminant<D>,
        D: PartialEq,
    {
        // Use empty template cell for resetting cells due to resize.
        let template = mem::take(&mut self.cursor.template);

        match self.lines.cmp(&lines) {
            Ordering::Less if top_anchored => self.grow_lines_top_anchored(lines),
            Ordering::Less => self.grow_lines(lines),
            Ordering::Greater => self.shrink_lines(lines),
            Ordering::Equal => (),
        }

        match self.columns.cmp(&columns) {
            Ordering::Less => self.grow_columns(reflow, columns),
            Ordering::Greater => self.shrink_columns(reflow, columns),
            Ordering::Equal => (),
        }

        // Restore template cell.
        self.cursor.template = template;
    }

    /// Add lines to the visible area.
    ///
    /// Alacritty keeps the cursor at the bottom of the terminal as long as there
    /// is scrollback available. Once scrollback is exhausted, new lines are
    /// simply added to the bottom of the screen.
    fn grow_lines<D>(&mut self, target: usize)
    where
        T: ResetDiscriminant<D>,
        D: PartialEq,
    {
        let lines_added = target - self.lines;

        // Need to resize before updating buffer.
        self.raw.grow_visible_lines(target);
        self.lines = target;

        let history_size = self.history_size();
        let from_history = min(history_size, lines_added);

        // Move existing lines up for every line that couldn't be pulled from history.
        if from_history != lines_added {
            let delta = lines_added - from_history;
            self.scroll_up(&(Line(0)..Line(target as i32)), delta);
        }

        // Move cursor down for every line pulled from history.
        self.saved_cursor.point.line += from_history;
        self.cursor.point.line += from_history;

        self.display_offset = self.display_offset.saturating_sub(lines_added);
        self.decrease_scroll_limit(lines_added);
    }

    /// Add lines to the visible area without touching history: the viewport
    /// grows downward — blank rows at the bottom, content and cursor fixed.
    /// The same path [`Self::grow_lines`] takes when history is empty, made
    /// unconditional (conhost/ConPTY semantics; see [`Self::resize_anchored`]).
    fn grow_lines_top_anchored<D>(&mut self, target: usize)
    where
        T: ResetDiscriminant<D>,
        D: PartialEq,
    {
        let lines_added = target - self.lines;

        // Need to resize before updating buffer.
        self.raw.grow_visible_lines(target);
        self.lines = target;

        // The enlarged viewport reaches up into history; rotate those rows
        // straight back out so fresh blank lines land at the bottom instead.
        self.scroll_up(&(Line(0)..Line(target as i32)), lines_added);

        self.display_offset = self.display_offset.saturating_sub(lines_added);
        self.decrease_scroll_limit(lines_added);
    }

    /// Remove lines from the visible area.
    ///
    /// The behavior in Terminal.app and iTerm.app is to keep the cursor at the
    /// bottom of the screen. This is achieved by pushing history "out the top"
    /// of the terminal window.
    ///
    /// Alacritty takes the same approach.
    fn shrink_lines<D>(&mut self, target: usize)
    where
        T: ResetDiscriminant<D>,
        D: PartialEq,
    {
        // Scroll up to keep content inside the window.
        let required_scrolling = (self.cursor.point.line.0 as usize + 1).saturating_sub(target);
        if required_scrolling > 0 {
            self.scroll_up(&(Line(0)..Line(self.lines as i32)), required_scrolling);

            // Clamp cursors to the new viewport size.
            self.cursor.point.line = min(self.cursor.point.line, Line(target as i32 - 1));
        }

        // Clamp saved cursor, since only primary cursor is scrolled into viewport.
        self.saved_cursor.point.line = min(self.saved_cursor.point.line, Line(target as i32 - 1));

        self.raw.rotate((self.lines - target) as isize);
        self.raw.shrink_visible_lines(target);
        self.lines = target;
    }

    /// Grow number of columns in each row, reflowing if necessary.
    fn grow_columns(&mut self, reflow: bool, columns: usize) {
        // Check if a row needs to be wrapped.
        let should_reflow = |row: &Row<T>| -> bool {
            let len = Column(row.len());
            reflow && len.0 > 0 && len < columns && row[len - 1].flags().contains(Flags::WRAPLINE)
        };

        self.columns = columns;

        let mut reversed: Vec<Row<T>> = Vec::with_capacity(self.raw.len());
        let mut cursor_line_delta = 0;

        // Remove the linewrap special case, by moving the cursor outside of the grid.
        if self.cursor.input_needs_wrap && reflow {
            self.cursor.input_needs_wrap = false;
            self.cursor.point.column += 1;
        }

        let mut rows = self.raw.take_all();

        for (i, mut row) in rows.drain(..).enumerate().rev() {
            // Check if reflowing should be performed.
            let last_row = match reversed.last_mut() {
                Some(last_row) if should_reflow(last_row) => last_row,
                _ => {
                    reversed.push(row);
                    continue;
                },
            };

            // Remove wrap flag before appending additional cells.
            if let Some(cell) = last_row.last_mut() {
                cell.flags_mut().remove(Flags::WRAPLINE);
            }

            // Remove leading spacers when reflowing wide char to the previous line.
            let mut last_len = last_row.len();
            if last_len >= 1
                && last_row[Column(last_len - 1)].flags().contains(Flags::LEADING_WIDE_CHAR_SPACER)
            {
                last_row.shrink(last_len - 1);
                last_len -= 1;
            }

            // Don't try to pull more cells from the next line than available.
            let mut num_wrapped = columns - last_len;
            let len = min(row.len(), num_wrapped);

            // Insert leading spacer when there's not enough room for reflowing wide char.
            let mut cells = if row[Column(len - 1)].flags().contains(Flags::WIDE_CHAR) {
                num_wrapped -= 1;

                let mut cells = row.front_split_off(len - 1);

                let mut spacer = T::default();
                spacer.flags_mut().insert(Flags::LEADING_WIDE_CHAR_SPACER);
                cells.push(spacer);

                cells
            } else {
                row.front_split_off(len)
            };

            // Add removed cells to previous row and reflow content.
            last_row.append(&mut cells);

            let cursor_buffer_line = self.lines - self.cursor.point.line.0 as usize - 1;

            if i == cursor_buffer_line && reflow {
                // Resize cursor's line and reflow the cursor if necessary.
                let mut target = self.cursor.point.sub(self, Boundary::Cursor, num_wrapped);

                // Clamp to the last column, if no content was reflown with the cursor.
                if target.column.0 == 0 && row.is_clear() {
                    self.cursor.input_needs_wrap = true;
                    target = target.sub(self, Boundary::Cursor, 1);
                }
                self.cursor.point.column = target.column;

                // Get required cursor line changes. Since `num_wrapped` is smaller than `columns`
                // this will always be either `0` or `1`.
                let line_delta = self.cursor.point.line - target.line;

                if line_delta != 0 && row.is_clear() {
                    continue;
                }

                cursor_line_delta += line_delta.0 as usize;
            } else if row.is_clear() {
                if i < self.display_offset {
                    // Since we removed a line, rotate down the viewport.
                    self.display_offset = self.display_offset.saturating_sub(1);
                }

                // Rotate cursor down if content below them was pulled from history.
                if i < cursor_buffer_line {
                    self.cursor.point.line += 1;
                }

                // Don't push line into the new buffer.
                continue;
            }

            if let Some(cell) = last_row.last_mut() {
                // Set wrap flag if next line still has cells.
                cell.flags_mut().insert(Flags::WRAPLINE);
            }

            reversed.push(row);
        }

        // Make sure we have at least the viewport filled.
        if reversed.len() < self.lines {
            let delta = (self.lines - reversed.len()) as i32;
            self.cursor.point.line = max(self.cursor.point.line - delta, Line(0));
            reversed.resize_with(self.lines, || Row::new(columns));
        }

        // Pull content down to put cursor in correct position, or move cursor up if there's no
        // more lines to delete below the cursor.
        if cursor_line_delta != 0 {
            let cursor_buffer_line = self.lines - self.cursor.point.line.0 as usize - 1;
            let available = min(cursor_buffer_line, reversed.len() - self.lines);
            let overflow = cursor_line_delta.saturating_sub(available);
            reversed.truncate(reversed.len() + overflow - cursor_line_delta);
            self.cursor.point.line = max(self.cursor.point.line - overflow, Line(0));
        }

        // Reverse iterator and fill all rows that are still too short.
        let mut new_raw = Vec::with_capacity(reversed.len());
        for mut row in reversed.drain(..).rev() {
            if row.len() < columns {
                row.grow(columns);
            }
            new_raw.push(row);
        }

        self.raw.replace_inner(new_raw);

        // Clamp display offset in case lines above it got merged.
        self.display_offset = min(self.display_offset, self.history_size());
    }

    /// Shrink number of columns in each row, reflowing if necessary.
    fn shrink_columns(&mut self, reflow: bool, columns: usize) {
        self.columns = columns;

        // Remove the linewrap special case, by moving the cursor outside of the grid.
        if self.cursor.input_needs_wrap && reflow {
            self.cursor.input_needs_wrap = false;
            self.cursor.point.column += 1;
        }

        let mut new_raw = Vec::with_capacity(self.raw.len());
        let mut buffered: Option<Vec<T>> = None;

        let mut rows = self.raw.take_all();
        for (i, mut row) in rows.drain(..).enumerate().rev() {
            // Append lines left over from the previous row.
            if let Some(buffered) = buffered.take() {
                // Add a column for every cell added before the cursor, if it goes beyond the new
                // width it is then later reflown.
                let cursor_buffer_line = self.lines - self.cursor.point.line.0 as usize - 1;
                if i == cursor_buffer_line {
                    self.cursor.point.column += buffered.len();
                }

                row.append_front(buffered);
            }

            loop {
                // Remove all cells which require reflowing.
                let mut wrapped = match row.shrink(columns) {
                    Some(wrapped) if reflow => wrapped,
                    _ => {
                        let cursor_buffer_line = self.lines - self.cursor.point.line.0 as usize - 1;
                        if reflow && i == cursor_buffer_line && self.cursor.point.column > columns {
                            // If there are empty cells before the cursor, we assume it is explicit
                            // whitespace and need to wrap it like normal content.
                            Vec::new()
                        } else {
                            // Since it fits, just push the existing line without any reflow.
                            new_raw.push(row);
                            break;
                        }
                    },
                };

                // Insert spacer if a wide char would be wrapped into the last column.
                if row.len() >= columns
                    && row[Column(columns - 1)].flags().contains(Flags::WIDE_CHAR)
                {
                    let mut spacer = T::default();
                    spacer.flags_mut().insert(Flags::LEADING_WIDE_CHAR_SPACER);

                    let wide_char = mem::replace(&mut row[Column(columns - 1)], spacer);
                    wrapped.insert(0, wide_char);
                }

                // Remove wide char spacer before shrinking.
                let len = wrapped.len();
                if len > 0 && wrapped[len - 1].flags().contains(Flags::LEADING_WIDE_CHAR_SPACER) {
                    if len == 1 {
                        row[Column(columns - 1)].flags_mut().insert(Flags::WRAPLINE);
                        new_raw.push(row);
                        break;
                    } else {
                        // Remove the leading spacer from the end of the wrapped row.
                        wrapped[len - 2].flags_mut().insert(Flags::WRAPLINE);
                        wrapped.truncate(len - 1);
                    }
                }

                new_raw.push(row);

                // Set line as wrapped if cells got removed.
                if let Some(cell) = new_raw.last_mut().and_then(|r| r.last_mut()) {
                    cell.flags_mut().insert(Flags::WRAPLINE);
                }

                if wrapped
                    .last()
                    .map(|c| c.flags().contains(Flags::WRAPLINE) && i >= 1)
                    .unwrap_or(false)
                    && wrapped.len() < columns
                {
                    // Make sure previous wrap flag doesn't linger around.
                    if let Some(cell) = wrapped.last_mut() {
                        cell.flags_mut().remove(Flags::WRAPLINE);
                    }

                    // Add removed cells to start of next row.
                    buffered = Some(wrapped);
                    break;
                } else {
                    // Reflow cursor if a line below it is deleted.
                    let cursor_buffer_line = self.lines - self.cursor.point.line.0 as usize - 1;
                    if (i == cursor_buffer_line && self.cursor.point.column < columns)
                        || i < cursor_buffer_line
                    {
                        self.cursor.point.line = max(self.cursor.point.line - 1, Line(0));
                    }

                    // Reflow the cursor if it is on this line beyond the width.
                    if i == cursor_buffer_line && self.cursor.point.column >= columns {
                        // Since only a single new line is created, we subtract only `columns`
                        // from the cursor instead of reflowing it completely.
                        self.cursor.point.column -= columns;
                    }

                    // Make sure new row is at least as long as new width.
                    let occ = wrapped.len();
                    if occ < columns {
                        wrapped.resize_with(columns, T::default);
                    }
                    row = Row::from_vec(wrapped, occ);

                    if i < self.display_offset {
                        // Since we added a new line, rotate up the viewport.
                        self.display_offset += 1;
                    }
                }
            }
        }

        // Reverse iterator and use it as the new grid storage.
        let mut reversed: Vec<Row<T>> = new_raw.drain(..).rev().collect();
        reversed.truncate(self.max_scroll_limit + self.lines);
        self.raw.replace_inner(reversed);

        // Clamp display offset in case some lines went off.
        self.display_offset = min(self.display_offset, self.history_size());

        // Reflow the primary cursor, or clamp it if reflow is disabled.
        if !reflow {
            self.cursor.point.column = min(self.cursor.point.column, Column(columns - 1));
        } else if self.cursor.point.column == columns
            && !self[self.cursor.point.line][Column(columns - 1)].flags().contains(Flags::WRAPLINE)
        {
            self.cursor.input_needs_wrap = true;
            self.cursor.point.column -= 1;
        } else {
            self.cursor.point = self.cursor.point.grid_clamp(self, Boundary::Cursor);
        }

        // Clamp the saved cursor to the grid.
        self.saved_cursor.point.column = min(self.saved_cursor.point.column, Column(columns - 1));
    }
}
