param([switch]$Loopback)
$ErrorActionPreference = 'Stop'
$taskProject = Split-Path -Parent $PSScriptRoot
$taskConfig = Get-Content -LiteralPath (Join-Path $env:LOCALAPPDATA 'AI Call Assistant\local-voice.json') -Raw | ConvertFrom-Json
$taskWav = Join-Path $taskConfig.dataDir 'practice-question.wav'
Add-Type -AssemblyName System.Speech
$taskVoice = New-Object System.Speech.Synthesis.SpeechSynthesizer
try {
    $taskFormat = New-Object System.Speech.AudioFormat.SpeechAudioFormatInfo -ArgumentList 16000,([System.Speech.AudioFormat.AudioBitsPerSample]::Sixteen),([System.Speech.AudioFormat.AudioChannel]::Mono)
    $taskVoice.SetOutputToWaveFile($taskWav,$taskFormat)
    $taskVoice.Speak('How do you handle a difficult customer?')
} finally { $taskVoice.Dispose() }
$env:CARGO_INCREMENTAL='0'
$env:CARGO_PROFILE_DEV_DEBUG='0'
$env:CARGO_PROFILE_TEST_DEBUG='0'
if (-not $env:CARGO_BUILD_JOBS) { $env:CARGO_BUILD_JOBS='1' }
$taskArgs=@('run','--manifest-path',(Join-Path $taskProject 'src-tauri\Cargo.toml'),'-p','app-core','--example','local_voice_smoke','--',$taskWav)
if ($Loopback) {
    Write-Host 'After compilation, the test will play a practice question through the default audio output to test WASAPI capture.'
    $taskArgs += '--loopback'
}
& cargo @taskArgs
if ($LASTEXITCODE -ne 0) { throw 'The real local voice test failed. See the error above.' }
