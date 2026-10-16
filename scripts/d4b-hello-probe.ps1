# SPDX-License-Identifier: FSL-1.1-Apache-2.0
#
# d4b-hello-probe.ps1 - what Windows Hello actually does on a real Windows computer, before
# AppRafter builds a Windows Hello way to open a target store (D.4b; ADR 0069, section 4).
#
# The owner runs it, once, on his own Windows computer, in Windows PowerShell 5.1 (powershell.exe,
# not pwsh: only 5.1 projects WinRT); an agent never runs it on a person's computer, and CI runs only
# its dry run. It needs no build, no admin rights and no AppRafter. It creates two throwaway keys of its own, named "apprafter-d4b-probe-*",
# and deletes them at the end (and at the start, if an earlier run left them). It never touches a
# target store, a password or any other key.
#
#   powershell.exe -NoProfile -ExecutionPolicy Bypass -File .\d4b-hello-probe.ps1
#   powershell.exe -NoProfile -ExecutionPolicy Bypass -File .\d4b-hello-probe.ps1 -SkipNcrypt
#   powershell.exe -NoProfile -ExecutionPolicy Bypass -File .\d4b-hello-probe.ps1 -DryRun
#
# -DryRun loads every type and compiles the NCrypt declarations, then stops: it calls no Windows
# Hello API at all (used to check the script itself on a CI runner).
#
# It asks a few yes/no questions (did a Windows Hello dialog appear just now?) and writes every
# answer and measurement to d4b-hello-probe-<time>.txt next to it. It prints SHA-256 digests only,
# never a signature, a public key or a decrypted secret. Rows:
#
#   P-W1  KeyCredentialManager.IsSupportedAsync
#   P-W2  create + sign: where the Hello dialog opened (this console window, in front or behind)
#   P-W3  two signatures over the same challenge are identical (deterministic)
#   P-W4  the signature verifies as RSASSA-PKCS1-v1_5 with SHA-256 under the key's own public half;
#         the modulus is at least 2048 bits and the signature as long as the modulus
#   P-W5  a second PowerShell process opens the same key by name and signs (same digest)
#   P-W6  what the dialog says (a screenshot the owner takes while it is open)
#   P-W7  DeleteAsync, then OpenAsync reports NotFound
#   P-W8  GetAttestationAsync: is the key kept in a TPM
#   P-W9  a signature from a second process after 1 s, 10 s and 60 s: did each ask a gesture
#   P-W10 OpenAsync + RetrievePublicKey, and DeleteAsync, open no dialog
#   P-W11 the NCrypt route: a Hello-gated RSA key of the Microsoft Passport key storage provider
#         that decrypts, with the "authentication mandatory" cache flag; its "use context" text

[CmdletBinding()]
param(
    [switch]$DryRun,
    [switch]$SkipNcrypt,
    # Internal: the second-process rows (P-W5, P-W9).
    [string]$Child = '',
    [string]$ChildName = '',
    [string]$ChildChallengeHex = ''
)

$ErrorActionPreference = 'Stop'
Set-StrictMode -Version 2

if ($PSVersionTable.PSEdition -ne 'Desktop') {
    Write-Host 'Run this in Windows PowerShell 5.1 (powershell.exe), not in PowerShell 7 (pwsh): only 5.1 can call the Windows Hello API from a script.' -ForegroundColor Red
    exit 2
}

# ---------------------------------------------------------------- WinRT plumbing

Add-Type -AssemblyName System.Runtime.WindowsRuntime
[void][Windows.Security.Credentials.KeyCredentialManager, Windows.Security.Credentials, ContentType = WindowsRuntime]
[void][Windows.Security.Credentials.KeyCredentialRetrievalResult, Windows.Security.Credentials, ContentType = WindowsRuntime]
[void][Windows.Security.Credentials.KeyCredentialOperationResult, Windows.Security.Credentials, ContentType = WindowsRuntime]
[void][Windows.Security.Credentials.KeyCredentialAttestationResult, Windows.Security.Credentials, ContentType = WindowsRuntime]
[void][Windows.Security.Cryptography.CryptographicBuffer, Windows.Security.Cryptography, ContentType = WindowsRuntime]
[void][Windows.Security.Cryptography.Core.CryptographicPublicKeyBlobType, Windows.Security.Cryptography.Core, ContentType = WindowsRuntime]
[void][Windows.Storage.Streams.IBuffer, Windows.Storage.Streams, ContentType = WindowsRuntime]

$script:AsTaskOp = [System.WindowsRuntimeSystemExtensions].GetMethods() | Where-Object {
    $_.Name -eq 'AsTask' -and $_.GetParameters().Count -eq 1 -and
    $_.GetParameters()[0].ParameterType.Name -eq 'IAsyncOperation`1'
} | Select-Object -First 1
$script:AsTaskAction = [System.WindowsRuntimeSystemExtensions].GetMethods() | Where-Object {
    $_.Name -eq 'AsTask' -and $_.GetParameters().Count -eq 1 -and
    $_.GetParameters()[0].ParameterType.Name -eq 'IAsyncAction'
} | Select-Object -First 1

function Await($Operation, [Type]$ResultType) {
    $task = $script:AsTaskOp.MakeGenericMethod($ResultType).Invoke($null, @($Operation))
    [void]$task.Wait(-1)
    return $task.Result
}

function Await-Action($Action) {
    $task = $script:AsTaskAction.Invoke($null, @($Action))
    [void]$task.Wait(-1)
}

$KCM = [Windows.Security.Credentials.KeyCredentialManager]
$CB = [Windows.Security.Cryptography.CryptographicBuffer]

function To-Bytes($Buffer) {
    $bytes = $null
    $CB::CopyToByteArray($Buffer, [ref]$bytes)
    return , $bytes
}

function Sha256Hex([byte[]]$Bytes) {
    $sha = [System.Security.Cryptography.SHA256]::Create()
    try { return ([BitConverter]::ToString($sha.ComputeHash($Bytes)) -replace '-', '').ToLowerInvariant() }
    finally { $sha.Dispose() }
}

function Hex-To-Bytes([string]$Hex) {
    $out = New-Object byte[] ($Hex.Length / 2)
    for ($i = 0; $i -lt $out.Length; $i++) { $out[$i] = [Convert]::ToByte($Hex.Substring(2 * $i, 2), 16) }
    return , $out
}

# ---------------------------------------------------------------- NCrypt (P-W11)

$ncryptSource = @'
using System;
using System.Runtime.InteropServices;
public static class D4bNcrypt {
    public const string MS_NGC_KEY_STORAGE_PROVIDER = "Microsoft Passport Key Storage Provider";
    public const uint NCRYPT_ALLOW_DECRYPT_FLAG = 0x1;
    public const uint NCRYPT_ALLOW_SIGNING_FLAG = 0x2;
    public const uint NCRYPT_NGC_CACHE_TYPE_PROPERTY_AUTH_MANDATORY_FLAG = 0x1;
    public const uint NCRYPT_PAD_PKCS1_FLAG = 0x2;
    public const uint NCRYPT_PAD_OAEP_FLAG = 0x4;
    public const uint NCRYPT_OVERWRITE_KEY_FLAG = 0x80;
    [StructLayout(LayoutKind.Sequential, CharSet = CharSet.Unicode)]
    public struct BCRYPT_OAEP_PADDING_INFO { public string pszAlgId; public IntPtr pbLabel; public int cbLabel; }
    [DllImport("ncrypt.dll", CharSet = CharSet.Unicode)]
    public static extern int NCryptOpenStorageProvider(out IntPtr phProvider, string pszProviderName, uint dwFlags);
    [DllImport("ncrypt.dll", CharSet = CharSet.Unicode)]
    public static extern int NCryptCreatePersistedKey(IntPtr hProvider, out IntPtr phKey, string pszAlgId, string pszKeyName, uint dwLegacyKeySpec, uint dwFlags);
    [DllImport("ncrypt.dll", CharSet = CharSet.Unicode)]
    public static extern int NCryptOpenKey(IntPtr hProvider, out IntPtr phKey, string pszKeyName, uint dwLegacyKeySpec, uint dwFlags);
    [DllImport("ncrypt.dll", CharSet = CharSet.Unicode)]
    public static extern int NCryptSetProperty(IntPtr hObject, string pszProperty, byte[] pbInput, int cbInput, uint dwFlags);
    [DllImport("ncrypt.dll", CharSet = CharSet.Unicode)]
    public static extern int NCryptSetProperty(IntPtr hObject, string pszProperty, IntPtr pbInput, int cbInput, uint dwFlags);
    [DllImport("ncrypt.dll")]
    public static extern int NCryptFinalizeKey(IntPtr hKey, uint dwFlags);
    [DllImport("ncrypt.dll", CharSet = CharSet.Unicode)]
    public static extern int NCryptExportKey(IntPtr hKey, IntPtr hExportKey, string pszBlobType, IntPtr pParameterList, byte[] pbOutput, int cbOutput, out int pcbResult, uint dwFlags);
    [DllImport("ncrypt.dll")]
    public static extern int NCryptDecrypt(IntPtr hKey, byte[] pbInput, int cbInput, ref BCRYPT_OAEP_PADDING_INFO pPaddingInfo, byte[] pbOutput, int cbOutput, out int pcbResult, uint dwFlags);
    [DllImport("ncrypt.dll", EntryPoint = "NCryptDecrypt")]
    public static extern int NCryptDecryptNoPad(IntPtr hKey, byte[] pbInput, int cbInput, IntPtr pPaddingInfo, byte[] pbOutput, int cbOutput, out int pcbResult, uint dwFlags);
    [DllImport("ncrypt.dll")]
    public static extern int NCryptDeleteKey(IntPtr hKey, uint dwFlags);
    [DllImport("ncrypt.dll")]
    public static extern int NCryptFreeObject(IntPtr hObject);
    [DllImport("kernel32.dll")]
    public static extern IntPtr GetConsoleWindow();
    public static int SetWindow(IntPtr key, IntPtr hwnd) {
        byte[] b = IntPtr.Size == 8 ? BitConverter.GetBytes(hwnd.ToInt64()) : BitConverter.GetBytes(hwnd.ToInt32());
        return NCryptSetProperty(key, "HWND Handle", b, b.Length, 0);
    }
    public static int SetString(IntPtr key, string name, string value) {
        byte[] b = System.Text.Encoding.Unicode.GetBytes(value + "\0");
        return NCryptSetProperty(key, name, b, b.Length, 0);
    }
    public static int SetUInt(IntPtr key, string name, uint value) {
        byte[] b = BitConverter.GetBytes(value);
        return NCryptSetProperty(key, name, b, b.Length, 0);
    }
}
'@
Add-Type -TypeDefinition $ncryptSource -Language CSharp
$N = [D4bNcrypt]

if ($DryRun) {
    Write-Host 'dry run: the WinRT types load, the await helpers resolve and the NCrypt declarations compile; no Windows Hello API was called.'
    if (-not $script:AsTaskOp -or -not $script:AsTaskAction) { Write-Host 'dry run: AsTask not found' -ForegroundColor Red; exit 1 }
    exit 0
}

# ---------------------------------------------------------------- the second process (P-W5, P-W9)

if ($Child -eq 'sign') {
    $open = Await ($KCM::OpenAsync($ChildName)) ([Windows.Security.Credentials.KeyCredentialRetrievalResult])
    if ($open.Status -ne 'Success') { Write-Output "CHILD open=$($open.Status)"; exit 0 }
    $sig = Await ($open.Credential.RequestSignAsync($CB::CreateFromByteArray((Hex-To-Bytes $ChildChallengeHex)))) ([Windows.Security.Credentials.KeyCredentialOperationResult])
    if ($sig.Status -ne 'Success') { Write-Output "CHILD sign=$($sig.Status)"; exit 0 }
    Write-Output ("CHILD sign=Success sha256=" + (Sha256Hex (To-Bytes $sig.Result)))
    exit 0
}

# ---------------------------------------------------------------- the main run

$stamp = Get-Date -Format 'yyyyMMdd-HHmmss'
$out = Join-Path (Split-Path -Parent $PSCommandPath) "d4b-hello-probe-$stamp.txt"
$rows = New-Object System.Collections.Generic.List[string]

function Row([string]$Id, [string]$Text) {
    $line = "$Id  $Text"
    $rows.Add($line)
    Write-Host $line -ForegroundColor Cyan
    Set-Content -Path $out -Value $rows -Encoding UTF8
}

function Ask([string]$Question) {
    while ($true) {
        $a = (Read-Host "$Question [y/n]").Trim().ToLowerInvariant()
        if ($a -eq 'y' -or $a -eq 'n') { return $a }
    }
}

# Any unexpected error ends the run with its text in the results file.
trap {
    try { Row 'ERROR' (($_ | Out-String).Trim()) } catch { }
    Write-Host "Results: $out"
    exit 1
}

function Pause-For([string]$Text) {
    Write-Host ''
    Write-Host $Text -ForegroundColor Yellow
    [void](Read-Host 'Press Enter to go on')
}

$hostName = if ($env:WT_SESSION) { 'Windows Terminal' } else { 'console host (conhost) or another terminal' }
Row 'INFO' ("Windows " + [Environment]::OSVersion.Version + ", PowerShell " + $PSVersionTable.PSVersion + ", " + $env:PROCESSOR_ARCHITECTURE + ", terminal: " + $hostName)
Row 'INFO' ("session: " + $env:SESSIONNAME + " (Console = at the computer; RDP-Tcp#N = a remote desktop)")

$name = 'apprafter-d4b-probe-sign'
$ngcName = 'apprafter-d4b-probe-decrypt'
# An earlier run may have left the keys.
try { Await-Action ($KCM::DeleteAsync($name)) } catch { }

# P-W1
$supported = Await ($KCM::IsSupportedAsync()) ([bool])
Row 'P-W1' "IsSupportedAsync = $supported"
if (-not $supported) {
    Row 'END' 'Windows Hello is not available here: set up a PIN, face or fingerprint in Settings > Accounts > Sign-in options, then run the probe again.'
    Write-Host "Results: $out"
    exit 0
}

# P-W2 / P-W6: create (it may already ask a gesture) and the first signature.
Pause-For 'Step 1: the probe creates a Windows Hello key. A Windows Hello dialog may appear. If it does, take a screenshot (Win+Shift+S) BEFORE you answer it, and save it as hello-create.png in the screenshot folder.'
$created = Await ($KCM::RequestCreateAsync($name, [Windows.Security.Credentials.KeyCredentialCreationOption]::FailIfExists)) ([Windows.Security.Credentials.KeyCredentialRetrievalResult])
Row 'P-W2' "create: status = $($created.Status)"
$askedAtCreate = Ask 'Did a Windows Hello dialog appear when the key was created?'
$whereCreate = if ($askedAtCreate -eq 'y') { Read-Host 'Where did it appear: in front of this window, behind it, or elsewhere? (type a few words)' } else { '-' }
Row 'P-W2' "create asked a gesture: $askedAtCreate; where: $whereCreate"
if ($created.Status -ne 'Success') {
    Row 'END' 'The key could not be created; the rest of the probe needs it.'
    Write-Host "Results: $out"
    exit 0
}
$cred = $created.Credential

$challenge = [byte[]](1..64 | ForEach-Object { Get-Random -Minimum 0 -Maximum 256 })
$challengeHex = ([BitConverter]::ToString($challenge) -replace '-', '').ToLowerInvariant()
$challengeBuf = $CB::CreateFromByteArray($challenge)

Pause-For 'Step 2: the probe asks Windows Hello to sign. Take a screenshot of the dialog (Win+Shift+S) BEFORE you answer it, save it as hello-sign.png, then approve it.'
$s1 = Await ($cred.RequestSignAsync($challengeBuf)) ([Windows.Security.Credentials.KeyCredentialOperationResult])
Row 'P-W2' "first signature: status = $($s1.Status)"
$askedSign1 = Ask 'Did a Windows Hello dialog appear for this signature?'
$whereSign = if ($askedSign1 -eq 'y') { Read-Host 'Where did it appear: in front of this window, behind it, or elsewhere? (type a few words)' } else { '-' }
Row 'P-W2' "first signature asked a gesture: $askedSign1; where: $whereSign"
$words = Read-Host 'P-W6: type the words the dialog showed (its title and the line under it), or "-"'
Row 'P-W6' "dialog words: $words (screenshots: hello-create.png, hello-sign.png)"
if ($s1.Status -ne 'Success') {
    Row 'END' 'The first signature failed; rows P-W3 to P-W10 need it.'
    try { Await-Action ($KCM::DeleteAsync($name)) } catch { }
    Write-Host "Results: $out"
    exit 0
}
$sig1 = To-Bytes $s1.Result

# P-W3
Pause-For 'Step 3: the probe signs the same challenge again (determinism). Approve the dialog if one appears.'
$s2 = Await ($cred.RequestSignAsync($challengeBuf)) ([Windows.Security.Credentials.KeyCredentialOperationResult])
$askedSign2 = Ask 'Did a Windows Hello dialog appear for this second signature?'
$sig2 = if ($s2.Status -eq 'Success') { To-Bytes $s2.Result } else { $null }
$same = ($null -ne $sig2) -and ((Sha256Hex $sig1) -eq (Sha256Hex $sig2))
Row 'P-W3' ("second signature: status = $($s2.Status); asked a gesture: $askedSign2; identical to the first: $same; sha256(sig1) = " + (Sha256Hex $sig1) + "; length = $($sig1.Length) bytes")

# P-W4: verify under the key's own public half.
$blobType = [Windows.Security.Cryptography.Core.CryptographicPublicKeyBlobType]
$spki = To-Bytes ($cred.RetrievePublicKey($blobType::X509SubjectPublicKeyInfo))
$bcrypt = To-Bytes ($cred.RetrievePublicKey($blobType::BCryptPublicKey))
$magic = [BitConverter]::ToUInt32($bcrypt, 0)
$bits = [BitConverter]::ToUInt32($bcrypt, 4)
$cbExp = [BitConverter]::ToUInt32($bcrypt, 8)
$cbMod = [BitConverter]::ToUInt32($bcrypt, 12)
$exp = $bcrypt[24..(24 + $cbExp - 1)]
$mod = $bcrypt[(24 + $cbExp)..(24 + $cbExp + $cbMod - 1)]
$rsa = New-Object System.Security.Cryptography.RSACng
$p = New-Object System.Security.Cryptography.RSAParameters
$p.Exponent = [byte[]]$exp
$p.Modulus = [byte[]]$mod
$rsa.ImportParameters($p)
$h = [System.Security.Cryptography.HashAlgorithmName]::SHA256
$okPkcs1 = $rsa.VerifyData($challenge, $sig1, $h, [System.Security.Cryptography.RSASignaturePadding]::Pkcs1)
$okPss = $false
try { $okPss = $rsa.VerifyData($challenge, $sig1, $h, [System.Security.Cryptography.RSASignaturePadding]::Pss) } catch { }
$okSha1 = $rsa.VerifyData($challenge, $sig1, [System.Security.Cryptography.HashAlgorithmName]::SHA1, [System.Security.Cryptography.RSASignaturePadding]::Pkcs1)
Row 'P-W4' ("BCRYPT magic = 0x{0:X8}; modulus = $bits bits ($cbMod bytes); signature length = $($sig1.Length); PKCS#1 v1.5 SHA-256 verifies: $okPkcs1; PSS SHA-256 verifies: $okPss; PKCS#1 v1.5 SHA-1 verifies: $okSha1; sha256(SPKI) = " -f $magic + (Sha256Hex $spki))

# P-W5 and P-W9: second processes.
$self = $PSCommandPath
function Child-Sign {
    $o = & powershell.exe -NoProfile -ExecutionPolicy Bypass -File $self -Child sign -ChildName $name -ChildChallengeHex $challengeHex 2>&1
    return ($o | Out-String).Trim()
}
Pause-For 'Step 4 (P-W5): a SECOND PowerShell process opens the same key by its name and signs. Approve the dialog if one appears.'
$c0 = Child-Sign
$askedC0 = Ask 'Did a Windows Hello dialog appear for the second process?'
$sameC0 = $c0 -match ('sha256=' + (Sha256Hex $sig1))
Row 'P-W5' "second process: $c0; asked a gesture: $askedC0; same signature as the first process: $sameC0"

foreach ($wait in 1, 10, 60) {
    Write-Host ''
    Write-Host "Step 5 (P-W9): the probe signs in the first process, waits $wait s, then signs from a second process. Approve each dialog that appears." -ForegroundColor Yellow
    [void](Read-Host 'Press Enter to go on')
    $sa = Await ($cred.RequestSignAsync($challengeBuf)) ([Windows.Security.Credentials.KeyCredentialOperationResult])
    $askedA = Ask 'Did a Windows Hello dialog appear for the first-process signature?'
    Start-Sleep -Seconds $wait
    $cb = Child-Sign
    $askedB = Ask "Did a Windows Hello dialog appear for the second-process signature, $wait s later?"
    Row 'P-W9' "after $wait s: first process status = $($sa.Status), asked: $askedA; second process: $cb, asked: $askedB"
}

# P-W8
$att = Await ($cred.GetAttestationAsync()) ([Windows.Security.Credentials.KeyCredentialAttestationResult])
$attLen = if ($att.AttestationBuffer) { (To-Bytes $att.AttestationBuffer).Length } else { 0 }
Row 'P-W8' "GetAttestationAsync status = $($att.Status) (Success: the key is in a TPM and attested; NotSupported: a software key or no attestation); attestation bytes = $attLen"
$askedAtt = Ask 'Did a Windows Hello dialog appear for the attestation?'
Row 'P-W8' "attestation asked a gesture: $askedAtt"

# P-W10: open + public key, without signing.
Pause-For 'Step 6 (P-W10): the probe opens the key and reads its public half, WITHOUT signing. Watch for any Windows Hello dialog.'
$re = Await ($KCM::OpenAsync($name)) ([Windows.Security.Credentials.KeyCredentialRetrievalResult])
$spki2 = if ($re.Status -eq 'Success') { To-Bytes ($re.Credential.RetrievePublicKey($blobType::X509SubjectPublicKeyInfo)) } else { $null }
$askedOpen = Ask 'Did any Windows Hello dialog appear while the key was opened?'
Row 'P-W10' ("open: status = $($re.Status); same public key: " + (($null -ne $spki2) -and ((Sha256Hex $spki2) -eq (Sha256Hex $spki))) + "; asked a gesture: $askedOpen")

# P-W7 / P-W10: delete.
Pause-For 'Step 7 (P-W7, P-W10): the probe deletes its key. Watch for any Windows Hello dialog.'
Await-Action ($KCM::DeleteAsync($name))
$askedDel = Ask 'Did any Windows Hello dialog appear while the key was deleted?'
$gone = Await ($KCM::OpenAsync($name)) ([Windows.Security.Credentials.KeyCredentialRetrievalResult])
Row 'P-W7' "after delete, OpenAsync status = $($gone.Status) (expected NotFound)"
Row 'P-W10' "delete asked a gesture: $askedDel"

# P-W11: the NCrypt Passport route.
if ($SkipNcrypt) {
    Row 'P-W11' 'skipped (-SkipNcrypt)'
} else {
    Pause-For 'Step 8 (P-W11): the probe creates a second key in the Microsoft Passport key storage provider that can only DECRYPT, gated by Windows Hello on every use, and asks it to decrypt 32 random bytes. Approve the dialogs; read their words.'
    $prov = [IntPtr]::Zero; $key = [IntPtr]::Zero
    $rc = $N::NCryptOpenStorageProvider([ref]$prov, $N::MS_NGC_KEY_STORAGE_PROVIDER, 0)
    $sid = [System.Security.Principal.WindowsIdentity]::GetCurrent().User.Value
    $keyName = "$sid//apprafter-d4b-probe//$ngcName"
    Row 'P-W11' ("open provider rc = 0x{0:X8}" -f $rc)
    if ($rc -eq 0) {
        $rc = $N::NCryptCreatePersistedKey($prov, [ref]$key, 'RSA', $keyName, 0, $N::NCRYPT_OVERWRITE_KEY_FLAG)
        Row 'P-W11' ("create rc = 0x{0:X8}" -f $rc)
        if ($rc -eq 0) {
            $rcLen = $N::SetUInt($key, 'Length', 2048)
            $rcUse = $N::SetUInt($key, 'Key Usage', $N::NCRYPT_ALLOW_DECRYPT_FLAG)
            $rcCache = $N::SetUInt($key, 'NgcCacheType', $N::NCRYPT_NGC_CACHE_TYPE_PROPERTY_AUTH_MANDATORY_FLAG)
            if ($rcCache -ne 0) { $rcCache2 = $N::SetUInt($key, 'NgcCacheTypeProperty', $N::NCRYPT_NGC_CACHE_TYPE_PROPERTY_AUTH_MANDATORY_FLAG) } else { $rcCache2 = 0 }
            $rcWin = $N::SetWindow($key, $N::GetConsoleWindow())
            $rcCtx = $N::SetString($key, 'Use Context', 'AppRafter probe: create a test key')
            $rcFin = $N::NCryptFinalizeKey($key, 0)
            Row 'P-W11' ("properties: length 0x{0:X8}, usage 0x{1:X8}, NgcCacheType 0x{2:X8} (fallback NgcCacheTypeProperty 0x{3:X8}), window 0x{4:X8}, use context 0x{5:X8}; finalize 0x{6:X8}" -f $rcLen, $rcUse, $rcCache, $rcCache2, $rcWin, $rcCtx, $rcFin)
            $askedNgcCreate = Ask 'Did a Windows Hello dialog appear when this key was created? (If its text says "AppRafter probe: create a test key", answer y and note it below.)'
            $ngcWords = Read-Host 'Type the words that dialog showed, or "-"'
            Row 'P-W11' "create asked a gesture: $askedNgcCreate; dialog words: $ngcWords"
            if ($rcFin -eq 0) {
                $len = 0
                [void]$N::NCryptExportKey($key, [IntPtr]::Zero, 'RSAPUBLICBLOB', [IntPtr]::Zero, $null, 0, [ref]$len, 0)
                $blob = New-Object byte[] $len
                $rcExp = $N::NCryptExportKey($key, [IntPtr]::Zero, 'RSAPUBLICBLOB', [IntPtr]::Zero, $blob, $len, [ref]$len, 0)
                $cbE = [BitConverter]::ToUInt32($blob, 8); $cbM = [BitConverter]::ToUInt32($blob, 12)
                $pp = New-Object System.Security.Cryptography.RSAParameters
                $pp.Exponent = [byte[]]$blob[24..(24 + $cbE - 1)]
                $pp.Modulus = [byte[]]$blob[(24 + $cbE)..(24 + $cbE + $cbM - 1)]
                $pub = New-Object System.Security.Cryptography.RSACng
                $pub.ImportParameters($pp)
                $secret = [byte[]](1..32 | ForEach-Object { Get-Random -Minimum 0 -Maximum 256 })
                Row 'P-W11' ("export rc = 0x{0:X8}; modulus = {1} bits" -f $rcExp, ($cbM * 8))
                foreach ($pad in 'OAEP-SHA256', 'PKCS1') {
                    if ($pad -eq 'OAEP-SHA256') { $ct = $pub.Encrypt($secret, [System.Security.Cryptography.RSAEncryptionPadding]::OaepSHA256) }
                    else { $ct = $pub.Encrypt($secret, [System.Security.Cryptography.RSAEncryptionPadding]::Pkcs1) }
                    foreach ($attempt in 'first', 'again after 5 s') {
                        if ($attempt -ne 'first') { Start-Sleep -Seconds 5 }
                        [void]$N::SetString($key, 'Use Context', "AppRafter probe: decrypt ($pad, $attempt)")
                        [void]$N::SetWindow($key, $N::GetConsoleWindow())
                        $pt = New-Object byte[] 512; $got = 0
                        if ($pad -eq 'OAEP-SHA256') {
                            $info = New-Object D4bNcrypt+BCRYPT_OAEP_PADDING_INFO
                            $info.pszAlgId = 'SHA256'; $info.pbLabel = [IntPtr]::Zero; $info.cbLabel = 0
                            $rcD = $N::NCryptDecrypt($key, $ct, $ct.Length, [ref]$info, $pt, $pt.Length, [ref]$got, $N::NCRYPT_PAD_OAEP_FLAG)
                        } else {
                            $rcD = $N::NCryptDecryptNoPad($key, $ct, $ct.Length, [IntPtr]::Zero, $pt, $pt.Length, [ref]$got, $N::NCRYPT_PAD_PKCS1_FLAG)
                        }
                        $match = ($rcD -eq 0) -and ((Sha256Hex ([byte[]]$pt[0..($got - 1)])) -eq (Sha256Hex $secret))
                        $askedD = Ask "Did a Windows Hello dialog appear for this decryption ($pad, $attempt)?"
                        Row 'P-W11' ("decrypt $pad ($attempt): rc = 0x{0:X8}; plaintext matches: $match; asked a gesture: $askedD" -f $rcD)
                    }
                }
                $ctxSeen = Read-Host 'Did the decryption dialogs show the text "AppRafter probe: decrypt (...)"? Type yes, no or what they showed'
                Row 'P-W11' "use-context text in the dialog: $ctxSeen"
            }
            $rcDel = $N::NCryptDeleteKey($key, 0)
            Row 'P-W11' ("delete rc = 0x{0:X8}" -f $rcDel)
        }
        [void]$N::NCryptFreeObject($prov)
    }
}

Row 'END' 'done. Paste this file into D4B_WALK_MAC_WIN.md (section "Pasted output") or into the chat.'
Write-Host ''
Write-Host "Results: $out" -ForegroundColor Green
