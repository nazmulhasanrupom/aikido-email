# AIKIDO EMAIL one-line installer for Windows (PowerShell 5.1 or 7).
#   irm https://raw.githubusercontent.com/nazmulhasanrupom/aikido-email/main/install.ps1 | iex
# Safe to run again: it keeps your .env and your data.
# It needs Windows 10 21H2 or newer, and Docker Desktop (with WSL 2).

function Install-Aikido {
    $repo = 'nazmulhasanrupom/aikido-email'
    $raw = if ($env:AIKIDO_RAW_URL) { $env:AIKIDO_RAW_URL } else { "https://raw.githubusercontent.com/$repo/main" }
    $dir = if ($env:AIKIDO_DIR) { $env:AIKIDO_DIR } else { Join-Path $env:USERPROFILE 'aikido-email' }
    $issues = "https://github.com/$repo/issues"
    $dockerLink = 'https://www.docker.com/products/docker-desktop/'

    function Step($text) { Write-Host "==> $text" -ForegroundColor Cyan }
    function Ok($text) { Write-Host " ok $text" -ForegroundColor Green }
    function Warn($text) { Write-Host "warn $text" -ForegroundColor Yellow }
    function Fail($text) { Write-Host "error $text" -ForegroundColor Red }

    # Run a program, hide its output, and say if it worked.
    function Works([string]$exe, [string[]]$arguments) {
        try {
            & $exe @arguments *> $null
            return ($LASTEXITCODE -eq 0)
        } catch {
            return $false
        }
    }

    Write-Host ''
    Write-Host '  AIKIDO EMAIL installer' -ForegroundColor White
    Write-Host '  Cold email automation. Self-hosted.'
    Write-Host ''

    # ---- Windows version ----
    $build = [Environment]::OSVersion.Version.Build
    if ($build -lt 19044) {
        Fail "This installer needs Windows 10 21H2 (build 19044) or newer, or Windows 11. Your build: $build."
        return
    }
    Step "System: Windows (build $build)"

    # ---- Docker ----
    $dockerExe = Join-Path $env:ProgramFiles 'Docker\Docker\resources\bin\docker.exe'
    if (-not (Get-Command docker -ErrorAction SilentlyContinue)) {
        if (Test-Path $dockerExe) {
            $env:Path = "$(Split-Path $dockerExe);$env:Path"
        } else {
            if (-not (Get-Command winget -ErrorAction SilentlyContinue)) {
                Fail "Docker is missing, and winget is not here. Install Docker Desktop: $dockerLink . Start it. Then run this installer again."
                return
            }
            if (-not (Works 'wsl' @('--status'))) {
                Fail 'Docker Desktop needs WSL 2, and WSL is not ready. Open PowerShell as administrator and run: wsl --install'
                Write-Host 'Restart the computer when it asks. Then run this installer again.'
                return
            }
            Step 'Installing Docker Desktop (Windows asks for administrator rights)'
            & winget install -e --id Docker.DockerDesktop --accept-source-agreements --accept-package-agreements
            if (-not (Test-Path $dockerExe)) {
                Fail "Docker Desktop did not install. Install it by hand: $dockerLink . Then run this installer again."
                return
            }
            $env:Path = "$(Split-Path $dockerExe);$env:Path"
            Warn 'Docker Desktop was just installed. If the next step fails, sign out and in (or restart), then run this installer again.'
        }
    }
    Ok 'Docker is installed'

    # ---- wait for Docker Desktop ----
    if (-not (Works 'docker' @('info'))) {
        $desktop = Join-Path $env:ProgramFiles 'Docker\Docker\Docker Desktop.exe'
        if (Test-Path $desktop) {
            Step 'Starting Docker Desktop'
            Start-Process $desktop
        }
        Step 'Waiting for Docker (up to 120 seconds)'
        $ready = $false
        for ($i = 0; $i -lt 60; $i++) {
            if (Works 'docker' @('info')) { $ready = $true; break }
            Start-Sleep -Seconds 2
        }
        if (-not $ready) {
            Fail 'Docker is installed but not running. Start Docker Desktop, wait until it says it is running, then run this installer again.'
            return
        }
    }
    if (-not (Works 'docker' @('compose', 'version'))) {
        Fail 'docker compose is missing. Update Docker Desktop.'
        return
    }
    Ok 'Docker is running'

    # ---- folder and compose file ----
    Step "Folder: $dir"
    New-Item -ItemType Directory -Force -Path $dir | Out-Null
    Set-Location $dir
    try {
        [Net.ServicePointManager]::SecurityProtocol = [Net.ServicePointManager]::SecurityProtocol -bor [Net.SecurityProtocolType]::Tls12
        Invoke-WebRequest -UseBasicParsing -Uri "$raw/docker-compose.install.yml" -OutFile 'docker-compose.yml.new' -ErrorAction Stop
        Move-Item -Force 'docker-compose.yml.new' 'docker-compose.yml'
    } catch {
        Fail "Could not download docker-compose.yml from $raw"
        return
    }
    Ok 'docker-compose.yml saved'

    # ---- .env (never overwritten) ----
    function Port-Busy([int]$port) {
        $listener = $null
        try {
            $listener = New-Object System.Net.Sockets.TcpListener([System.Net.IPAddress]::Any, $port)
            $listener.Start()
            return $false
        } catch {
            return $true
        } finally {
            if ($listener) { $listener.Stop() }
        }
    }
    function Random-Bytes([int]$count) {
        $bytes = New-Object byte[] $count
        $rng = [System.Security.Cryptography.RandomNumberGenerator]::Create()
        $rng.GetBytes($bytes)
        $rng.Dispose()
        return $bytes
    }

    $envFile = Join-Path $dir '.env'
    if (Test-Path $envFile) {
        Ok '.env already exists. I keep it as it is.'
    } else {
        Step 'Making .env with random secrets'
        $port = 8080
        while (Port-Busy $port) { $port++ }
        if ($port -ne 8080) { Warn "Port 8080 is busy. Using port $port." }
        $password = -join ((Random-Bytes 24) | ForEach-Object { $_.ToString('x2') })
        $key = [Convert]::ToBase64String((Random-Bytes 32))
        $lines = @(
            'POSTGRES_USER=aikido',
            "POSTGRES_PASSWORD=$password",
            'POSTGRES_DB=aikido',
            "AIKIDO_ENCRYPTION_KEY=$key",
            "APP_PORT=$port",
            "AIKIDO_PUBLIC_URL=http://localhost:$port"
        )
        # UTF-8 without a BOM, LF line ends: Docker compose reads a BOM badly.
        $utf8 = New-Object System.Text.UTF8Encoding($false)
        [System.IO.File]::WriteAllText($envFile, (($lines -join "`n") + "`n"), $utf8)
        Ok '.env saved. Back up AIKIDO_ENCRYPTION_KEY from it.'
    }
    $settings = @{}
    foreach ($line in Get-Content $envFile) {
        if ($line -match '^\s*([A-Za-z_][A-Za-z0-9_]*)=(.*)$') { $settings[$Matches[1]] = $Matches[2].Trim() }
    }
    $port = if ($settings['APP_PORT']) { $settings['APP_PORT'] } else { '8080' }
    $url = if ($settings['AIKIDO_PUBLIC_URL']) { $settings['AIKIDO_PUBLIC_URL'] } else { "http://localhost:$port" }

    # ---- start ----
    Step 'Downloading the images'
    & docker compose pull
    if ($LASTEXITCODE -ne 0) {
        Fail 'docker compose pull failed.'
        Write-Host "Need help? $issues"
        return
    }
    Step 'Starting'
    & docker compose up -d
    if ($LASTEXITCODE -ne 0) {
        Fail 'docker compose up failed.'
        & docker compose logs --tail 50
        Write-Host "Need help? $issues"
        return
    }

    # ---- wait for health ----
    Step 'Waiting for the app (up to 60 seconds)'
    $healthy = $false
    for ($i = 0; $i -lt 30; $i++) {
        try {
            $r = Invoke-WebRequest -UseBasicParsing -Uri "http://127.0.0.1:$port/health" -TimeoutSec 3 -ErrorAction Stop
            if ($r.StatusCode -eq 200) { $healthy = $true; break }
        } catch {
            # not ready yet
        }
        Start-Sleep -Seconds 2
    }

    # ---- result ----
    if ($healthy) {
        Write-Host ''
        Write-Host 'AIKIDO EMAIL is running.' -ForegroundColor Green
        Write-Host ''
        Write-Host "  Open: $url"
        Write-Host ''
        Write-Host '  The first account you make becomes the owner.'
        Write-Host '  Make it right now, before you share the address with anyone.'
        Write-Host ''
        Write-Host "  stop:      cd $dir; docker compose down"
        Write-Host "  start:     cd $dir; docker compose up -d"
        Write-Host "  update:    cd $dir; docker compose pull; docker compose up -d"
        Write-Host "  logs:      cd $dir; docker compose logs -f app"
        Write-Host "  uninstall: & `"$dir\uninstall.ps1`""
        Write-Host ''
        try { Invoke-WebRequest -UseBasicParsing -Uri "$raw/uninstall.ps1" -OutFile (Join-Path $dir 'uninstall.ps1') -ErrorAction Stop } catch { Warn 'Could not download uninstall.ps1.' }
        Start-Process $url
    } else {
        Write-Host ''
        Fail 'The app did not become healthy in 60 seconds. Last 50 log lines:'
        & docker compose logs --tail 50
        Write-Host ''
        Write-Host "Need help? $issues"
    }
}

Install-Aikido
