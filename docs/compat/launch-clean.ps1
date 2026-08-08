# Launch a terminal-under-test with a minimal PATH and a neutral CWD.
#
# Why: LoadLibrary("conpty.dll") searches the process CWD and PATH. A terminal
# that ships no pair of its own (stock alacritty, rio) will silently pick up
# conpty.dll from ANY directory on PATH — installing WezTerm re-hosted both on
# this machine (see README "PATH-walk hazard"). PATH hygiene therefore cannot
# live inside the probe script: by the time bash runs, the host was chosen at
# terminal launch. It has to live HERE, on the launching side.
#
# A terminal's own install dir is searched first, so pairs shipped next to the
# exe (RikkaTerminal, WezTerm) still win — which is the behaviour under test.
#
#   pwsh -File launch-clean.ps1 <terminal.exe> [args...]
#
# Pair with the probe's "live OpenConsole hosts:" line, which stamps the
# surviving host paths into the screenshot itself.
param(
    [Parameter(Mandatory = $true)][string]$Exe,
    [Parameter(ValueFromRemainingArguments = $true)][string[]]$Rest
)
$ErrorActionPreference = 'Stop'

$env:PATH = "$env:SystemRoot\System32;$env:SystemRoot;$env:SystemRoot\System32\WindowsPowerShell\v1.0"

$neutral = Join-Path $env:TEMP 'probe-neutral-cwd'
New-Item -ItemType Directory -Force -Path $neutral | Out-Null

if ($Rest -and $Rest.Count -gt 0) {
    Start-Process -FilePath $Exe -ArgumentList $Rest -WorkingDirectory $neutral
} else {
    Start-Process -FilePath $Exe -WorkingDirectory $neutral
}
