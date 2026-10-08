# Installs the Ahrara CLI on Windows:
#   iwr https://ahrara.dev/install.ps1 -useb | iex
# Downloads the release binary listed in the manifest's direct channel, checks
# its size and SHA-256, adds it to the user PATH and registers the installation
# for `ahrara update`. AHRARA_INSTALL_DIR and AHRARA_MANIFEST_URL override the
# defaults. Errors throw instead of calling exit, which would close the window.
& {
    $ErrorActionPreference = 'Stop'
    $ProgressPreference = 'SilentlyContinue'
    [Net.ServicePointManager]::SecurityProtocol = [Net.ServicePointManager]::SecurityProtocol -bor [Net.SecurityProtocolType]::Tls12

    $manifestUrl = if ($env:AHRARA_MANIFEST_URL) { $env:AHRARA_MANIFEST_URL } else { 'https://github.com/Hitmasu/Ahrara/releases/latest/download/stable.json' }
    if ($manifestUrl -notlike 'https://*') { throw 'Ahrara install: the manifest URL must use https' }
    $dest = if ($env:AHRARA_INSTALL_DIR) { $env:AHRARA_INSTALL_DIR } else { Join-Path $env:LOCALAPPDATA 'Programs\Ahrara' }
    $exe = Join-Path $dest 'ahrara.exe'
    $receipt = "$exe.install.json"

    # The operating system's architecture, not this PowerShell process's, which may be emulated.
    try { $arch = [Runtime.InteropServices.RuntimeInformation]::OSArchitecture.ToString() } catch { $arch = $env:PROCESSOR_ARCHITECTURE }
    $target = switch ($arch) {
        { $_ -in 'X64', 'AMD64' } { 'windows-x86_64' }
        'Arm64' { 'windows-aarch64' }
        default { throw "Ahrara install: unsupported architecture $arch" }
    }

    # The CLI records which installer owns it beside the executable.
    if (Test-Path $receipt) {
        $method = (Get-Content -Raw $receipt | ConvertFrom-Json).method
        if ($method -ne 'powershell') { throw "Ahrara install: $exe is managed by $method; update it there instead" }
        & $exe update
        if ($LASTEXITCODE) { throw 'Ahrara install: the update failed' }
        return
    }

    $work = Join-Path ([IO.Path]::GetTempPath()) ([IO.Path]::GetRandomFileName())
    $download = $null
    New-Item -ItemType Directory -Force -Path $work, $dest | Out-Null
    try {
        $manifest = Join-Path $work 'stable.json'
        Invoke-WebRequest -UseBasicParsing -Uri $manifestUrl -OutFile $manifest
        $envelope = Get-Content -Raw $manifest | ConvertFrom-Json
        $payload = [Text.Encoding]::UTF8.GetString([Convert]::FromBase64String($envelope.payload)) | ConvertFrom-Json
        $direct = $payload.channels.direct
        $artifact = $direct.artifacts.$target
        if (-not $artifact) { throw "Ahrara install: no Ahrara $($direct.version) release for $target yet" }
        if ($artifact.url -notlike 'https://*') { throw 'Ahrara install: the manifest has an invalid download URL' }

        Write-Host "Downloading Ahrara $($direct.version) for $target"
        $download = Join-Path $dest "ahrara-$([IO.Path]::GetRandomFileName()).download"
        Invoke-WebRequest -UseBasicParsing -Uri $artifact.url -OutFile $download
        if ((Get-Item $download).Length -ne $artifact.size) { throw 'Ahrara install: the download size does not match the manifest' }
        if ((Get-FileHash -Algorithm SHA256 $download).Hash -ne $artifact.sha256) { throw 'Ahrara install: the download checksum does not match the manifest' }
        Move-Item -Force $download $exe
        $download = $null
    } finally {
        Remove-Item -Recurse -Force $work -ErrorAction SilentlyContinue
        if ($download) { Remove-Item -Force $download -ErrorAction SilentlyContinue }
    }

    & $exe update --register powershell | Out-Null
    if ($LASTEXITCODE) { throw 'Ahrara install: cannot register the installation' }
    & $exe --version

    $userPath = [Environment]::GetEnvironmentVariable('Path', 'User')
    if (-not (($userPath -split ';') -contains $dest)) {
        [Environment]::SetEnvironmentVariable('Path', $(if ($userPath) { "$userPath;$dest" } else { $dest }), 'User')
        $env:Path = "$env:Path;$dest"
        Write-Host "Added $dest to your PATH. Open a new PowerShell window to use ahrara."
    }
}
