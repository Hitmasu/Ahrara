# Run in a Visual Studio Developer PowerShell with x64 and ARM64 C++ build tools.
$ErrorActionPreference = 'Stop'
$ProjectRoot = Split-Path $PSScriptRoot -Parent
Push-Location (Join-Path $ProjectRoot 'cli')
try {
    New-Item -ItemType Directory -Force (Join-Path $ProjectRoot 'dist') | Out-Null
    foreach ($Architecture in @('x86_64', 'aarch64')) {
        $Target = "$Architecture-pc-windows-msvc"
        rustup target add $Target --toolchain 1.98.1
        if ($LASTEXITCODE -ne 0) { throw 'Unable to install Rust target' }
        cargo build --locked --release --target $Target --bin ahrara
        if ($LASTEXITCODE -ne 0) { throw "Build failed for $Target" }
        Copy-Item "target/$Target/release/ahrara.exe" (Join-Path $ProjectRoot "dist/ahrara-windows-$Architecture.exe")
    }
} finally { Pop-Location }
