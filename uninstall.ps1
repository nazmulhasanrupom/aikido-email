# Stops AIKIDO EMAIL on Windows. Asks before it deletes your data.
#   & "$env:USERPROFILE\aikido-email\uninstall.ps1"

function Uninstall-Aikido {
    $dir = if ($env:AIKIDO_DIR) { $env:AIKIDO_DIR } else { Join-Path $env:USERPROFILE 'aikido-email' }
    if (-not (Test-Path (Join-Path $dir 'docker-compose.yml'))) {
        Write-Host "Nothing found in $dir." -ForegroundColor Red
        return
    }
    Set-Location $dir

    & docker compose down
    Write-Host 'AIKIDO EMAIL is stopped. Your data is still here.'

    $answer = Read-Host 'Delete database too? (y/N)'
    if ($answer -notmatch '^(y|yes)$') {
        Write-Host "Data kept in $dir and in the Docker volumes."
        return
    }
    $sure = Read-Host "This deletes the database and the folder $dir. Type DELETE to go on"
    if ($sure -cne 'DELETE') {
        Write-Host 'Nothing was deleted.'
        return
    }
    & docker compose down -v
    Set-Location $env:USERPROFILE
    Remove-Item -Recurse -Force $dir
    Write-Host "Database and $dir are deleted."
}

Uninstall-Aikido
