# Hashi installer for Windows.
#
#   irm https://raw.githubusercontent.com/sidharthpunathil/hashi/main/install.ps1 | iex
#
# Options (environment variables):
#   $env:HASHI_VERSION = "v0.4.4"      install a specific release (default: latest)
#   $env:HASHI_INSTALL_DIR = "C:\bin"  where to put hashi.exe
#                                      (default: %LOCALAPPDATA%\Programs\hashi\bin)
#   $env:HASHI_NO_DOCTOR = "1"         skip the post-install `hashi doctor` check
#   $env:HASHI_CONNECT = '<invite>'    after installing, run `hashi connect` with
#                                      this invite link (instead of `hashi doctor`)
#
# It downloads a prebuilt binary from GitHub Releases, verifies its SHA-256
# checksum, installs it for the current user and adds it to your user PATH.
# No administrator rights are needed. If no prebuilt binary fits this machine,
# it falls back to `cargo install`.

function Install-Hashi {
    $ErrorActionPreference = 'Stop'
    $ProgressPreference = 'SilentlyContinue'   # Invoke-WebRequest is much faster without it
    # Windows PowerShell 5.1 defaults to old TLS versions that GitHub rejects.
    [Net.ServicePointManager]::SecurityProtocol = [Net.ServicePointManager]::SecurityProtocol -bor [Net.SecurityProtocolType]::Tls12

    # Glyphs are built from code points so this file stays pure ASCII, which
    # Windows PowerShell 5.1 parses correctly without a byte order mark.
    $spark = [char]0x2726; $check = [char]0x2713; $arrow = [char]0x203A; $dot = [char]0x00B7

    $repo = 'sidharthpunathil/hashi'
    # An invite link of exactly the shape the claim page builds (SAFE_ORIGIN
    # and SAFE_CODE in src/web/hashi.js, joined by /claim/). Nothing else
    # reaches `hashi connect`, so no quote or space can split the argument
    # Windows PowerShell 5.1 builds for it.
    $connectPattern = '^(?:https://(?:[A-Za-z0-9](?:[A-Za-z0-9.-]{0,251}[A-Za-z0-9])?|\[[0-9A-Fa-f:.]+\])|http://(?:localhost|127(?:\.[0-9]{1,3}){3}|\[::1\]))(?::[0-9]{1,5})?/claim/hashi_inv_[0-9a-f]{16,118}\z'
    if ($env:HASHI_CONNECT -and ($env:HASHI_CONNECT -cnotmatch $connectPattern)) {
        throw 'HASHI_CONNECT is not an invite link like https://<host>/claim/hashi_inv_<code>; copy the command from the claim page again.'
    }
    $version = if ($env:HASHI_VERSION) { $env:HASHI_VERSION } else { 'latest' }
    $installDir = if ($env:HASHI_INSTALL_DIR) { $env:HASHI_INSTALL_DIR } else { Join-Path $env:LOCALAPPDATA 'Programs\hashi\bin' }

    Write-Host ''
    Write-Host "  $spark hashi " -ForegroundColor Magenta -NoNewline
    Write-Host 'the bridge to Claude Code and Codex' -ForegroundColor DarkGray
    Write-Host ''

    # x64 builds also run on Windows on ARM through emulation.
    $arch = $env:PROCESSOR_ARCHITECTURE
    if ($arch -notin @('AMD64', 'ARM64')) {
        throw "Hashi needs 64-bit Windows (found $arch)."
    }
    if ($arch -eq 'ARM64') {
        Write-Host "  $dot using the x64 build through Windows emulation" -ForegroundColor DarkGray
    }
    $target = 'x86_64-pc-windows-msvc'
    $asset = "hashi-$target.zip"
    $base = if ($version -eq 'latest') {
        "https://github.com/$repo/releases/latest/download"
    } else {
        "https://github.com/$repo/releases/download/$version"
    }

    $tmp = Join-Path ([IO.Path]::GetTempPath()) ("hashi-" + [Guid]::NewGuid().ToString('N'))
    New-Item -ItemType Directory -Path $tmp | Out-Null
    try {
        New-Item -ItemType Directory -Force -Path $installDir | Out-Null
        $exe = Join-Path $installDir 'hashi.exe'

        Write-Host "  $arrow downloading $asset ($version)" -ForegroundColor Magenta
        $zip = Join-Path $tmp $asset
        $downloaded = $true
        try {
            Invoke-WebRequest -UseBasicParsing -Uri "$base/$asset" -OutFile $zip
        } catch {
            $downloaded = $false
        }
        if ($downloaded) {
            # The archive exists, so never fall back to an unverified build:
            # a missing or wrong checksum is an error.
            try {
                Invoke-WebRequest -UseBasicParsing -Uri "$base/$asset.sha256" -OutFile "$zip.sha256"
            } catch {
                throw "Could not download the checksum for $asset; refusing to install."
            }
        }

        if ($downloaded) {
            $expected = ((Get-Content "$zip.sha256" -Raw).Trim() -split '\s+')[0].ToLower()
            $actual = (Get-FileHash $zip -Algorithm SHA256).Hash.ToLower()
            if (-not $expected -or $expected -ne $actual) {
                throw "Checksum mismatch for $asset; refusing to install."
            }
            Write-Host "  $check checksum verified" -ForegroundColor Green

            Expand-Archive -Path $zip -DestinationPath $tmp -Force
            Copy-Item (Join-Path $tmp 'hashi.exe') $exe -Force
        } else {
            Write-Host "  $dot no prebuilt binary found, building from source" -ForegroundColor DarkGray
            if (-not (Get-Command cargo -ErrorAction SilentlyContinue)) {
                throw 'No prebuilt binary is available and cargo is not installed. Install Rust from https://rustup.rs and run this again.'
            }
            $cargoArgs = @('install', '--locked', '--git', "https://github.com/$repo", '--root', (Join-Path $tmp 'cargo'))
            if ($version -ne 'latest') { $cargoArgs += @('--tag', $version) }
            & cargo @cargoArgs
            if ($LASTEXITCODE -ne 0) { throw 'cargo install failed.' }
            Copy-Item (Join-Path $tmp 'cargo\bin\hashi.exe') $exe -Force
        }

        $installed = (& $exe --version)
        Write-Host "  $check installed $installed to $installDir" -ForegroundColor Green

        # Put Hashi on the user PATH (persisted) and on this session's PATH.
        $userPath = [Environment]::GetEnvironmentVariable('Path', 'User')
        $parts = @($userPath -split ';' | Where-Object { $_ })
        if ($parts -notcontains $installDir) {
            [Environment]::SetEnvironmentVariable('Path', (($parts + $installDir) -join ';'), 'User')
            Write-Host "  $check added to your PATH (new terminals pick it up automatically)" -ForegroundColor Green
        }
        if (($env:Path -split ';') -notcontains $installDir) {
            $env:Path = "$env:Path;$installDir"
        }

        if ($env:HASHI_CONNECT) {
            # A friend's setup: redeem the invite link instead of checking for
            # local agents, which they do not need. The link is spent either
            # way, so it is cleared from this session afterwards.
            $link = $env:HASHI_CONNECT
            Remove-Item Env:HASHI_CONNECT -ErrorAction SilentlyContinue
            Write-Host "  $arrow connecting with your invite link" -ForegroundColor Magenta
            # Splatted, so '--' reaches hashi as a plain argument and the link
            # can never be read as an option.
            $connectArgs = @('connect', '--', $link)
            & $exe @connectArgs
            if ($LASTEXITCODE -ne 0) {
                throw "Hashi is installed, but connecting with the invite link failed (the reason is above). An invite link works once: if it was already used or has expired, ask for a new one, then run: hashi connect -- '<new link>'"
            }
            Write-Host ''
            return
        }

        if (-not $env:HASHI_NO_DOCTOR) {
            & $exe doctor
        }

        Write-Host '  Next: ' -NoNewline
        Write-Host 'hashi' -ForegroundColor Magenta -NoNewline
        Write-Host ' starts the bridge and opens the dashboard.'
        if (-not (Get-Command claude -ErrorAction SilentlyContinue) -and -not (Get-Command codex -ErrorAction SilentlyContinue)) {
            Write-Host '  Install and sign in to Claude Code or Codex first; Hashi drives them for you.' -ForegroundColor DarkGray
        }
        Write-Host ''
    } finally {
        Remove-Item -Recurse -Force $tmp -ErrorAction SilentlyContinue
    }
}

Install-Hashi
