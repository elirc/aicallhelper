param([switch]$SkipWarm)
$ErrorActionPreference = 'Stop'
$taskConfig = Join-Path $env:LOCALAPPDATA 'AI Call Assistant\local-voice.json'
if (-not (Test-Path -LiteralPath $taskConfig)) { throw 'Run setup-free-voice.ps1 first.' }
$taskHome = (Get-Content -LiteralPath $taskConfig -Raw | ConvertFrom-Json).dataDir
function Test-LocalService([string]$Uri) {
    try { return Invoke-RestMethod -Uri $Uri -TimeoutSec 3 } catch { return $null }
}
$taskOllama = Test-LocalService 'http://127.0.0.1:11434/api/tags'
if ($null -eq $taskOllama) {
    $env:OLLAMA_HOST = '127.0.0.1:11434'
    $env:OLLAMA_NO_CLOUD = '1'
    $env:OLLAMA_MODELS = Join-Path $taskHome 'models\ollama'
    Start-Process -FilePath (Join-Path $taskHome 'ollama\ollama.exe') -ArgumentList 'serve' -WindowStyle Hidden -WorkingDirectory $taskHome -RedirectStandardError (Join-Path $taskHome 'ollama.log') | Out-Null
}
$taskSpeech = Test-LocalService 'http://127.0.0.1:8765/health'
if ($null -ne $taskSpeech -and $taskSpeech.service -ne 'callhelper-local-speech') { throw 'Port 8765 is in use by another service.' }
if ($null -eq $taskSpeech) {
    $taskServer = Join-Path $taskHome 'server.py'
    $taskArgs = '-u "' + $taskServer + '" --home "' + $taskHome + '"'
    Start-Process -FilePath (Join-Path $taskHome 'venv\Scripts\python.exe') -ArgumentList $taskArgs -WindowStyle Hidden -WorkingDirectory $taskHome -RedirectStandardError (Join-Path $taskHome 'speech.log') | Out-Null
}
$taskDeadline = [DateTime]::UtcNow.AddSeconds(60)
do {
    $taskOllama = Test-LocalService 'http://127.0.0.1:11434/api/tags'
    $taskSpeech = Test-LocalService 'http://127.0.0.1:8765/health'
    if ($null -ne $taskOllama -and $taskSpeech.ready -eq $true) { break }
    Start-Sleep -Milliseconds 500
} while ([DateTime]::UtcNow -lt $taskDeadline)
if ($null -eq $taskOllama -or $taskSpeech.ready -ne $true) { throw "Local services did not start. Check speech.log and ollama.log in $taskHome" }
if (-not $SkipWarm) {
    $taskBody = @{model='qwen3.5:2b';messages=@();stream=$false;think=$false;keep_alive='10m';options=@{num_ctx=8192;num_thread=4}} | ConvertTo-Json -Depth 5
    try {
    $taskWarm = Invoke-RestMethod -Uri 'http://127.0.0.1:11434/api/chat' -Method Post -ContentType 'application/json' -Body $taskBody -TimeoutSec 120
    } catch {
        throw "Qwen could not finish loading. Close unused apps to free several GB of RAM, then rerun this script. Check ollama.log in $taskHome for details. Original error: $($_.Exception.Message)"
    }
    if ($taskWarm.done -ne $true) { throw 'The answer model did not finish warming.' }
}
Write-Host 'Local speech and Ollama are running on this computer.'
