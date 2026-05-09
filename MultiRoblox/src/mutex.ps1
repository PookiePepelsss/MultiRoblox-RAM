$null = New-Object Threading.Mutex($true, 'ROBLOX_singletonMutex')
$null = New-Object Threading.Mutex($true, 'ROBLOX_singletonEvent')
Write-Host 'MUTEX_HELD'
while ($true) { Start-Sleep 3600 }
