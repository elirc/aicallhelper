param(
    [string]$DataDir = (Join-Path $env:LOCALAPPDATA 'AI Call Assistant\local-voice'),
    [string]$Python = 'python'
)
$ErrorActionPreference = 'Stop'
$taskProject = Split-Path -Parent $PSScriptRoot
$DataDir = [System.IO.Path]::GetFullPath($DataDir)
New-Item -ItemType Directory -Path $DataDir -Force | Out-Null
$taskDrive = [System.IO.DriveInfo]::new([System.IO.Path]::GetPathRoot($DataDir))
$taskModelManifest = Join-Path $DataDir 'models\ollama\manifests\registry.ollama.ai\library\qwen3.5\2b'
$taskAlreadyInstalled = (Test-Path -LiteralPath $taskModelManifest) -and (Test-Path -LiteralPath (Join-Path $DataDir 'ollama\callhelper-runtime.json'))
$taskRequiredSpace = if ($taskAlreadyInstalled) { 512MB } else { 4GB }
if ($taskDrive.AvailableFreeSpace -lt $taskRequiredSpace) {
    throw "Free voice setup needs $([Math]::Round($taskRequiredSpace / 1GB, 1)) GB free for this operation (8 GB recommended for a first install). Use -DataDir on another drive or free space first."
}
& $Python -c "import sys; assert sys.version_info >= (3, 11), 'Python 3.11 or newer is required'"
if ($LASTEXITCODE -ne 0) { throw 'Install 64-bit Python 3.11 or newer, then rerun setup.' }
$taskVenv = Join-Path $DataDir 'venv'
$taskPy = Join-Path $taskVenv 'Scripts\python.exe'
if (-not (Test-Path -LiteralPath $taskPy)) {
    & $Python -m venv $taskVenv
    if ($LASTEXITCODE -ne 0) { throw 'Could not create the local Python environment.' }
}
& $taskPy -m ensurepip --upgrade
if ($LASTEXITCODE -ne 0) { throw 'Could not prepare pip in the local environment.' }
& $taskPy -m pip install --no-cache-dir -r (Join-Path $taskProject 'local-voice\requirements.txt')
if ($LASTEXITCODE -ne 0) { throw 'Local speech dependencies did not install.' }
Copy-Item -LiteralPath (Join-Path $taskProject 'local-voice\server.py') -Destination (Join-Path $DataDir 'server.py') -Force
& $taskPy (Join-Path $taskProject 'local-voice\install_ollama.py') --home $DataDir
if ($LASTEXITCODE -ne 0) { throw 'Portable Ollama installation did not finish.' }
& $taskPy (Join-Path $DataDir 'server.py') --home $DataDir --download-only
if ($LASTEXITCODE -ne 0) { throw 'The English speech model did not download.' }

$taskConfigDir = Join-Path $env:LOCALAPPDATA 'AI Call Assistant'
New-Item -ItemType Directory -Path $taskConfigDir -Force | Out-Null
$taskConfig = Join-Path $taskConfigDir 'local-voice.json'
@{dataDir=$DataDir} | ConvertTo-Json | Set-Content -LiteralPath ($taskConfig + '.tmp') -Encoding UTF8
Move-Item -LiteralPath ($taskConfig + '.tmp') -Destination $taskConfig -Force
& (Join-Path $PSScriptRoot 'start-free-voice.ps1') -SkipWarm

$env:OLLAMA_HOST = '127.0.0.1:11434'
$env:OLLAMA_NO_CLOUD = '1'
$env:OLLAMA_MODELS = Join-Path $DataDir 'models\ollama'
$taskOllama = Join-Path $DataDir 'ollama\ollama.exe'
Write-Host 'Downloading Qwen3.5 2B into the running local Ollama service (about 2.7 GB)...'
& $taskOllama pull qwen3.5:2b
if ($LASTEXITCODE -ne 0) { throw 'Qwen3.5 download did not finish. Check disk space and rerun setup.' }
& (Join-Path $PSScriptRoot 'start-free-voice.ps1')
Write-Host 'Free voice is ready. In Settings select Free local voice, Save, and return to Record.'
