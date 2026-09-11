# Build a fully private child-process environment for reference/candidate test runs.
# Usage:  powershell -NoProfile -File scripts\port-isolation.ps1 -Command "prime-agent --version"
param(
  [Parameter(Mandatory=$true)][string]$Command,
  [string]$Root = "C:\Users\openclawuser\optimus-rust-port\.port-env\iso",
  [string]$KernelPython = "C:\Users\openclawuser\optimus-rust-port\.port-env\python\Scripts\python.exe",
  [string]$PythonPath = "C:\Users\openclawuser\optimus-rust-port\prime-agent-runtime\src"
)
$ErrorActionPreference = "Stop"
foreach ($d in @("agent","sessions","artifacts","memory","harness","cache","tmp","pipes","logs","kernel","home")) {
  New-Item -ItemType Directory -Force -Path (Join-Path $Root $d) | Out-Null
}
# Refuse to run if the root resolves outside the project (junction/symlink escape check).
$resolved = (Resolve-Path $Root).Path
if (-not $resolved.StartsWith("C:\Users\openclawuser\optimus-rust-port\")) {
  throw "Isolation root escaped the project: $resolved"
}

$env:PRIME_AGENT_CODING_AGENT_DIR   = Join-Path $Root "agent"
$env:PI_CODING_AGENT_DIR            = Join-Path $Root "agent"
$env:PRIME_AGENT_SESSION_DIR        = Join-Path $Root "sessions"
$env:PRIME_AGENT_CODING_AGENT_SESSION_DIR = Join-Path $Root "sessions"
$env:PRIME_AGENT_KERNEL_PYTHON      = $KernelPython
$env:PRIME_AGENT_KERNEL_VENV        = Join-Path $Root "kernel"
$env:PYTHONPATH                     = $PythonPath
$env:TEMP                           = Join-Path $Root "tmp"
$env:TMP                            = Join-Path $Root "tmp"
$env:HOME                           = Join-Path $Root "home"
$env:USERPROFILE                    = Join-Path $Root "home"
$env:PRIME_AGENT_TELEMETRY          = "0"
$env:PRIME_AGENT_INTERNAL_SESSION_LEASES = "1"
$env:PRIME_AGENT_INTERNAL_SESSION_LEASE_OWNER_ID = "rust-port-test"
# Never let an inherited production guard load into a port process.
Remove-Item Env:NODE_OPTIONS -ErrorAction SilentlyContinue

# Hard stop if anything still points at production state.
foreach ($v in @("PRIME_AGENT_CODING_AGENT_DIR","PI_CODING_AGENT_DIR","PRIME_AGENT_SESSION_DIR","TEMP","TMP","HOME","USERPROFILE")) {
  $val = [Environment]::GetEnvironmentVariable($v)
  if ($val -and $val -like "*\.prime\*") { throw "$v still points at production state: $val" }
}

Invoke-Expression $Command
exit $LASTEXITCODE
