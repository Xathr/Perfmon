# Builds the perfmon plugin on Windows: target\wasm32-wasip2\release\Perfmon.wasm
$ErrorActionPreference = "Stop"
$Here = $PSScriptRoot
if (-not (Get-Command cargo -ErrorAction SilentlyContinue)) { throw "cargo not found: install Rust from https://rustup.rs" }
if (-not ((& rustup target list --installed) -contains "wasm32-wasip2")) { & rustup target add wasm32-wasip2 }
Push-Location $Here
try { & cargo build --release --target wasm32-wasip2; if ($LASTEXITCODE -ne 0) { throw "build failed" } } finally { Pop-Location }
Get-Item "$Here\target\wasm32-wasip2\release\Perfmon.wasm"
