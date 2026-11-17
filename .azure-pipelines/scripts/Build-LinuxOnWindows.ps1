$ErrorActionPreference = 'Stop'
if ($env:AGENT_OS -ne 'Windows_NT' -or $env:AGENT_OSARCHITECTURE -ne 'X64') {
    throw 'Linux cross-compilation requires a Windows x64 agent.'
}
$target = 'x86_64-unknown-linux-gnu'
$zigVersion = '0.14.1'
$zigbuildVersion = '0.24.1'
$tools = Join-Path $env:AGENT_TEMPDIRECTORY 'linux-cross-tools'
$archive = Join-Path $tools 'zig.zip'
New-Item -ItemType Directory -Path $tools | Out-Null
Invoke-WebRequest "https://ziglang.org/download/$zigVersion/zig-x86_64-windows-$zigVersion.zip" `
    -OutFile $archive
$expected = '554f5378228923ffd558eac35e21af020c73789d87afeabf4bfd16f2e6feed2c'
if ((Get-FileHash -Algorithm SHA256 $archive).Hash.ToLowerInvariant() -cne $expected) {
    throw 'Zig download does not match the pinned official SHA-256; refusing to execute it.'
}
Expand-Archive -LiteralPath $archive -DestinationPath $tools
$zig = Join-Path $tools "zig-x86_64-windows-$zigVersion/zig.exe"
$env:CARGO_ZIGBUILD_ZIG_PATH = $zig
& $zig version
if ($LASTEXITCODE -ne 0) { throw 'Cannot execute the pinned Zig compiler.' }
& rustup target add --toolchain $env:RUSTUP_TOOLCHAIN $target
if ($LASTEXITCODE -ne 0) { throw 'Cannot install the Linux Rust target.' }
& cargo install cargo-zigbuild --version $zigbuildVersion --locked --root $tools `
    --config .cargo/config.toml
if ($LASTEXITCODE -ne 0) { throw 'Cannot install pinned cargo-zigbuild through the authenticated Cargo feed.' }
& "$tools/bin/cargo-zigbuild.exe" zigbuild --release --locked -p rayfin-local `
    --target "$target.2.28"
if ($LASTEXITCODE -ne 0) { throw 'Windows-to-Linux cross-compilation failed.' }
& node .github/scripts/unix-release.mjs package linux
if ($LASTEXITCODE -ne 0) { throw 'Linux ELF validation and archive packaging failed.' }
Write-Host 'Native Linux execution tests are deferred; the artifact records nativeExecutionTested: false.'
