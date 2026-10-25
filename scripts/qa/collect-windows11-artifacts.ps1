# Build-only collector. It never starts a generated libtest or shipping binary.
[CmdletBinding()]
param(
    [Parameter(Mandatory)][string]$RepositoryRoot,
    [Parameter(Mandatory)][string]$OutputDirectory,
    [Parameter(Mandatory)][string]$WorkflowHead
)

Set-StrictMode -Version Latest
$ErrorActionPreference = 'Stop'
$ProductHead = 'ba8211e63307231482ee8f6f4c44ae58ad4d89e4'
$ProductTree = '68ed32d05b22a222edb22260dc55870acc4b8c84'
$TargetTriple = 'x86_64-pc-windows-msvc'
$Scope = 'WINDOWS11_NATIVE18_LIBTEST_ONLY'
$RepositoryRoot = [IO.Path]::GetFullPath($RepositoryRoot)
$OutputDirectory = [IO.Path]::GetFullPath($OutputDirectory)
$ToolsRoot = [IO.Path]::GetFullPath($PSScriptRoot)
$TargetRoot = Join-Path $RepositoryRoot 'target'
$Git = (Get-Command git.exe -CommandType Application -ErrorAction Stop).Source

function Require([bool]$Condition, [string]$Message) {
    if (-not $Condition) { throw $Message }
}
function Assert-Under([string]$Path, [string]$Root) {
    $full = [IO.Path]::GetFullPath($Path)
    $prefix = [IO.Path]::GetFullPath($Root).TrimEnd([char[]]'\/') + [IO.Path]::DirectorySeparatorChar
    Require ($full.StartsWith($prefix, [StringComparison]::OrdinalIgnoreCase)) 'Path escapes its owned root'
}
function Assert-NormalPath([string]$Path, [bool]$File) {
    $full = [IO.Path]::GetFullPath($Path)
    $part = [IO.Path]::GetPathRoot($full)
    foreach ($name in $full.Substring($part.Length).Split([char[]]'\/', [StringSplitOptions]::RemoveEmptyEntries)) {
        $part = Join-Path $part $name
        $item = Get-Item -LiteralPath $part -Force -ErrorAction Stop
        Require (($item.Attributes -band [IO.FileAttributes]::ReparsePoint) -eq 0) 'Reparse points are not permitted'
    }
    if ($File) { Require ((Get-Item -LiteralPath $full -Force) -is [IO.FileInfo]) 'Expected a normal file' }
}
function File-Stat([string]$Path) {
    Assert-NormalPath $Path $true
    $f = [IO.FileInfo]::new($Path)
    $f.Refresh()
    return [ordered]@{ path = $f.FullName; bytes = $f.Length; attributes = [int]$f.Attributes; creation_utc_ticks = $f.CreationTimeUtc.Ticks; last_write_utc_ticks = $f.LastWriteTimeUtc.Ticks }
}
function Same-Stat($Before, $After) {
    Require ((ConvertTo-Json -InputObject $Before -Compress) -ceq (ConvertTo-Json -InputObject $After -Compress)) 'File changed during collection'
}
function File-Sha([string]$Path) {
    $before = File-Stat $Path
    $stream = [IO.File]::Open($Path, [IO.FileMode]::Open, [IO.FileAccess]::Read, [IO.FileShare]::Read)
    $hash = [Security.Cryptography.SHA256]::Create()
    try { $value = [Convert]::ToHexString($hash.ComputeHash($stream)).ToLowerInvariant() }
    finally { $hash.Dispose(); $stream.Dispose() }
    Same-Stat $before (File-Stat $Path)
    return $value
}
function Copy-Verified([string]$Source, [string]$Destination) {
    $before = File-Stat $Source
    $sourceSha = File-Sha $Source
    Require (-not (Test-Path -LiteralPath $Destination)) 'Refusing to overwrite evidence'
    [void][IO.Directory]::CreateDirectory([IO.Path]::GetDirectoryName($Destination))
    [IO.File]::Copy($Source, $Destination, $false)
    $copyBefore = File-Stat $Destination
    $copySha = File-Sha $Destination
    $after = File-Stat $Source
    Same-Stat $before $after
    Same-Stat $copyBefore (File-Stat $Destination)
    Require ($sourceSha -ceq $copySha -and $sourceSha -ceq (File-Sha $Source)) 'Source and copied bytes differ'
    return [ordered]@{ sha256 = $copySha; bytes = $copyBefore.bytes; source_before = $before; source_after = $after; copy_stat = $copyBefore }
}
function Write-Json([string]$Path, $Data) {
    [IO.File]::WriteAllText($Path, (ConvertTo-Json -InputObject $Data -Depth 80) + "`n", [Text.UTF8Encoding]::new($false))
}
function Read-Json([string]$Path) {
    Assert-NormalPath $Path $true
    return ConvertFrom-Json -InputObject ([IO.File]::ReadAllText($Path, [Text.UTF8Encoding]::new($false, $true))) -AsHashtable -Depth 100
}
function Git-Text([string[]]$Arguments) {
    $text = & $Git -C $RepositoryRoot @Arguments
    Require ($LASTEXITCODE -eq 0) 'Read-only Git query failed'
    return ($text -join "`n").Trim()
}
function Git-Blob([string]$Path) {
    $bytes = [IO.File]::ReadAllBytes($Path)
    $prefix = [Text.Encoding]::ASCII.GetBytes("blob $($bytes.Length)`0")
    $stream = [IO.MemoryStream]::new()
    $hash = [Security.Cryptography.SHA1]::Create()
    try {
        $stream.Write($prefix, 0, $prefix.Length)
        $stream.Write($bytes, 0, $bytes.Length)
        $stream.Position = 0
        return [Convert]::ToHexString($hash.ComputeHash($stream)).ToLowerInvariant()
    } finally { $hash.Dispose(); $stream.Dispose() }
}
function Same-Strings($Actual, $Expected) {
    $left = @($Actual | Sort-Object -CaseSensitive)
    $right = @($Expected | Sort-Object -CaseSensitive)
    Require ($left.Count -eq $right.Count) 'Unexpected feature or target set'
    for ($i = 0; $i -lt $left.Count; $i++) { Require ($left[$i] -ceq $right[$i]) 'Unexpected feature or target value' }
}
function Invoke-Recorded([string]$Executable, [string[]]$Arguments, [string]$Stdout, [string]$Stderr) {
    Require (-not (Test-Path -LiteralPath $Stdout) -and -not (Test-Path -LiteralPath $Stderr)) 'Command logs already exist'
    $info = [Diagnostics.ProcessStartInfo]::new()
    $info.FileName = $Executable
    $info.WorkingDirectory = $RepositoryRoot
    $info.UseShellExecute = $false
    $info.RedirectStandardOutput = $true
    $info.RedirectStandardError = $true
    foreach ($arg in $Arguments) { [void]$info.ArgumentList.Add($arg) }
    $process = [Diagnostics.Process]::new()
    $process.StartInfo = $info
    $outFile = [IO.File]::Open($Stdout, [IO.FileMode]::CreateNew)
    $errFile = [IO.File]::Open($Stderr, [IO.FileMode]::CreateNew)
    try {
        Require ($process.Start()) 'Native command failed to start'
        $outTask = $process.StandardOutput.BaseStream.CopyToAsync($outFile)
        $errTask = $process.StandardError.BaseStream.CopyToAsync($errFile)
        $process.WaitForExit()
        $outTask.GetAwaiter().GetResult()
        $errTask.GetAwaiter().GetResult()
        $exitCode = $process.ExitCode
    } finally { $outFile.Dispose(); $errFile.Dispose(); $process.Dispose() }
    Require ($exitCode -eq 0) "Native build/probe failed with exit $exitCode; no artifact may be published"
    return [ordered]@{ executable = $Executable; argv = $Arguments; natural_exit = $exitCode; stdout_sha256 = File-Sha $Stdout; stderr_sha256 = File-Sha $Stderr }
}
function Assert-Pe64([string]$Path) {
    Assert-NormalPath $Path $true
    $stream = [IO.File]::Open($Path, [IO.FileMode]::Open, [IO.FileAccess]::Read, [IO.FileShare]::Read)
    $reader = [IO.BinaryReader]::new($stream)
    try {
        Require ($stream.Length -ge 64 -and $reader.ReadUInt16() -eq 0x5a4d) 'Expected a PE executable'
        $stream.Position = 0x3c
        $offset = $reader.ReadInt32()
        Require ($offset -ge 64 -and $offset -le $stream.Length - 26) 'Invalid PE header offset'
        $stream.Position = $offset
        Require ($reader.ReadUInt32() -eq 0x4550 -and $reader.ReadUInt16() -eq 0x8664) 'Expected PE x64 machine'
        $stream.Position = $offset + 24
        Require ($reader.ReadUInt16() -eq 0x20b) 'Expected PE32+ optional header'
    } finally { $reader.Dispose(); $stream.Dispose() }
}
function Assert-Source() {
    Require ((Git-Text @('rev-parse', 'HEAD')) -ceq $ProductHead) 'Product HEAD changed'
    Require ((Git-Text @('rev-parse', 'HEAD^{tree}')) -ceq $ProductTree) 'Product tree changed'
    Require ([string]::IsNullOrEmpty((Git-Text @('status', '--porcelain=v1', '--untracked-files=no')))) 'Tracked product source changed'
    Require ((File-Sha (Join-Path $RepositoryRoot 'Cargo.lock')) -ceq $script:LockSha) 'Cargo.lock changed'
    foreach ($entry in $script:Inventory.source_files.GetEnumerator()) {
        $path = Join-Path $RepositoryRoot $entry.Key
        Require ((File-Sha $path) -ceq $entry.Value.sha256) 'Working source SHA differs from the fixed inventory'
        Require ((File-Stat $path).bytes -eq $entry.Value.bytes) 'Working source byte count differs'
        Require ((Git-Blob $path) -ceq $entry.Value.git_blob) 'Working source bytes are not the fixed Git blob'
        Require ((Git-Text @('rev-parse', "HEAD:$($entry.Key)")) -ceq $entry.Value.git_blob) 'HEAD source blob differs'
    }
    foreach ($entry in $script:Inventory.inputs.GetEnumerator()) {
        $path = Join-Path $RepositoryRoot $entry.Key
        Require ((File-Sha $path) -ceq $entry.Value.sha256 -and (File-Stat $path).bytes -eq $entry.Value.bytes) 'Build input working bytes differ'
        Require ((Git-Blob $path) -ceq $entry.Value.git_blob -and (Git-Text @('rev-parse', "HEAD:$($entry.Key)")) -ceq $entry.Value.git_blob) 'Build input Git blob differs'
    }
}
function Assets-Manifest() {
    $root = Join-Path $RepositoryRoot 'crates/duduclaw-dashboard/dist'
    Assert-NormalPath $root $false
    $files = [ordered]@{}
    foreach ($entry in Get-ChildItem -LiteralPath $root -Force -Recurse | Sort-Object -Property FullName) {
        Assert-NormalPath $entry.FullName (-not $entry.PSIsContainer)
        if ($entry.PSIsContainer) { continue }
        $relative = [IO.Path]::GetRelativePath($root, $entry.FullName).Replace('\', '/')
        $files[$relative] = [ordered]@{ bytes = $entry.Length; sha256 = File-Sha $entry.FullName }
    }
    Require ($files.Count -gt 1 -and $files.Contains('index.html')) 'Real dashboard build output is missing'
    return [ordered]@{ source_head = $ProductHead; web_git_tree = Git-Text @('rev-parse', 'HEAD:web'); package_lock_sha256 = File-Sha (Join-Path $RepositoryRoot 'web/package-lock.json'); files = $files }
}

Require ($IsWindows -and [Environment]::Is64BitProcess) 'The builder requires native x64 Windows'
Require ($WorkflowHead -cmatch '^[0-9a-f]{40}$' -and $WorkflowHead -ceq $env:GITHUB_SHA -and $WorkflowHead -cne $ProductHead) 'Workflow commit is not its distinct github.sha'
Require ($env:GITHUB_EVENT_NAME -ceq 'push' -and $env:GITHUB_REF -ceq 'refs/heads/chore/windows11-acceptance-artifacts-20261025') 'Builder event/ref is not authorized'
Require ($env:GITHUB_RUN_ID -cmatch '^\d+$' -and $env:GITHUB_RUN_ATTEMPT -cmatch '^\d+$') 'Invalid workflow run identity'
Require ($RepositoryRoot -ieq [IO.Path]::GetFullPath($env:GITHUB_WORKSPACE)) 'Unexpected repository root'
Assert-Under $ToolsRoot $env:RUNNER_TEMP
Assert-Under $OutputDirectory $env:RUNNER_TEMP
Require ([IO.Path]::GetFileName($OutputDirectory) -ceq "native18-ba8211e6-$env:GITHUB_RUN_ID-$env:GITHUB_RUN_ATTEMPT") 'Unexpected artifact directory'
Require (-not (Test-Path -LiteralPath $OutputDirectory)) 'One-shot artifact directory already exists'
Assert-NormalPath $RepositoryRoot $false
Assert-NormalPath $ToolsRoot $false
foreach ($key in @('RUSTFLAGS', 'CARGO_ENCODED_RUSTFLAGS', 'RUST_MIN_STACK', 'RUSTC_WRAPPER', 'RUSTC_WORKSPACE_WRAPPER')) {
    Require ([string]::IsNullOrEmpty([Environment]::GetEnvironmentVariable($key, 'Process'))) "Build variant is forbidden: $key"
}
foreach ($entry in @{ CARGO_BUILD_JOBS = '1'; CARGO_PROFILE_DEV_DEBUG = '0'; CARGO_PROFILE_TEST_DEBUG = '0'; CARGO_PROFILE_DEV_INCREMENTAL = 'false'; CARGO_PROFILE_TEST_INCREMENTAL = 'false'; CARGO_INCREMENTAL = '0' }.GetEnumerator()) {
    Require ([Environment]::GetEnvironmentVariable($entry.Key, 'Process') -ceq $entry.Value) "Unexpected build profile environment: $($entry.Key)"
}
$env:CARGO_TARGET_DIR = $TargetRoot
$Inventory = Read-Json (Join-Path $ToolsRoot 'cases.json')
Require ($Inventory.schema -ceq 'duduclaw-native18-cases-v1' -and $Inventory.scope -ceq $Scope) 'Wrong portable case inventory'
Require ($Inventory.source.head -ceq $ProductHead -and $Inventory.source.tree -ceq $ProductTree -and $Inventory.target -ceq $TargetTriple) 'Wrong case source or target'
Require ($Inventory.source_files.Count -eq 7 -and $Inventory.cases.Count -eq 18 -and @($Inventory.cases.name | Sort-Object -Unique).Count -eq 18) 'Expected exactly seven source files and eighteen unique names'
Require ($Inventory.inputs.Count -eq 7) 'Expected the fixed Cargo and frontend input inventory'
foreach ($group in @{ 'cli-libtest' = 7; 'gateway-libtest' = 4; 'fork-libtest' = 7 }.GetEnumerator()) {
    Require (@($Inventory.cases | Where-Object { $_.artifact -ceq $group.Key }).Count -eq $group.Value) 'Native case group counts differ'
}
$LockSha = $Inventory.inputs['Cargo.lock'].sha256
$workflowSource = Read-Json (Join-Path $ToolsRoot 'workflow-source.json')
Require ($workflowSource.head -ceq $WorkflowHead -and $workflowSource.files.Count -eq 3) 'Workflow source receipt does not bind its own commit'
foreach ($name in @('collector.ps1', 'cases.json', 'workflow.yml')) {
    Require ((File-Sha (Join-Path $ToolsRoot $name)) -ceq $workflowSource.files[$name].sha256) 'Staged builder tool changed after checkout'
}
Assert-Source
[void][IO.Directory]::CreateDirectory($OutputDirectory)
$evidence = [Collections.Generic.List[string]]::new()
foreach ($name in @('workflow.yml', 'workflow-source.json', 'collector.ps1', 'cases.json', 'node-version.txt', 'npm-version.txt', 'frontend-build.json')) {
    $null = Copy-Verified (Join-Path $ToolsRoot $name) (Join-Path $OutputDirectory $name)
    $evidence.Add($name)
}
$frontend = Read-Json (Join-Path $OutputDirectory 'frontend-build.json')
Require ($frontend.source_head -ceq $ProductHead -and $frontend.stub -is [bool] -and -not $frontend.stub) 'Frontend receipt does not describe the fixed real build'
Same-Strings $frontend.natural_exit_codes @(0, 0)
foreach ($name in @('node-version.txt', 'npm-version.txt')) {
    Require (-not [string]::IsNullOrWhiteSpace([IO.File]::ReadAllText((Join-Path $OutputDirectory $name)))) 'Tool version evidence is empty'
}
$sourceProofs = [ordered]@{}
foreach ($entry in $Inventory.source_files.GetEnumerator()) {
    Require ($entry.Key -cmatch '^crates/[A-Za-z0-9_./-]+\.rs$' -and -not $entry.Key.Split('/').Contains('..')) 'Invalid source evidence path'
    $relative = 'source/' + $entry.Key
    $sourceProofs[$entry.Key] = Copy-Verified (Join-Path $RepositoryRoot $entry.Key) (Join-Path $OutputDirectory $relative)
    $evidence.Add($relative)
}
$Rustc = (Get-Command rustc.exe -CommandType Application -ErrorAction Stop).Source
$Cargo = (Get-Command cargo.exe -CommandType Application -ErrorAction Stop).Source
$toolRecords = [ordered]@{}
foreach ($spec in @(@{ id = 'rustc'; exe = $Rustc }, @{ id = 'cargo'; exe = $Cargo })) {
    $stdout = "$($spec.id)-vV.txt"
    $stderr = "$($spec.id)-vV.stderr.txt"
    $toolRecords[$spec.id] = Invoke-Recorded $spec.exe @('-vV') (Join-Path $OutputDirectory $stdout) (Join-Path $OutputDirectory $stderr)
    $evidence.Add($stdout); $evidence.Add($stderr)
}
Require (([IO.File]::ReadAllText((Join-Path $OutputDirectory 'rustc-vV.txt'))) -cmatch '(?m)^host: x86_64-pc-windows-msvc\r?$') 'Unexpected Rust host target'
Write-Json (Join-Path $OutputDirectory 'toolchain.json') ([ordered]@{ schema = 'native18-toolchain-v1'; target = $TargetTriple; rustc = @{ path = $Rustc; version_file = 'rustc-vV.txt'; version_sha256 = File-Sha (Join-Path $OutputDirectory 'rustc-vV.txt') }; cargo = @{ path = $Cargo; version_file = 'cargo-vV.txt'; version_sha256 = File-Sha (Join-Path $OutputDirectory 'cargo-vV.txt') }; node = @{ version_file = 'node-version.txt'; version_sha256 = File-Sha (Join-Path $OutputDirectory 'node-version.txt') }; npm = @{ version_file = 'npm-version.txt'; version_sha256 = File-Sha (Join-Path $OutputDirectory 'npm-version.txt') }; probes = $toolRecords; powershell_version = $PSVersionTable.PSVersion.ToString(); runner_os = $env:RUNNER_OS; runner_arch = $env:RUNNER_ARCH })
$evidence.Add('toolchain.json')
$assetsBefore = Assets-Manifest
Write-Json (Join-Path $OutputDirectory 'assets-manifest.json') $assetsBefore
$evidence.Add('assets-manifest.json')
$artifacts = [Collections.Generic.List[object]]::new()
$commands = [Collections.Generic.List[object]]::new()
$specs = @(
    @{ id = 'cli-libtest'; package = 'duduclaw-cli'; target_name = 'duduclaw_cli'; features = @('dashboard', 'default'); no_default = $false },
    @{ id = 'gateway-libtest'; package = 'duduclaw-gateway'; target_name = 'duduclaw_gateway'; features = @(); no_default = $true },
    @{ id = 'fork-libtest'; package = 'duduclaw-fork'; target_name = 'duduclaw_fork'; features = @(); no_default = $false }
)
foreach ($spec in $specs) {
    Assert-Source
    $argv = @('test', '--locked', '-p', $spec.package, '--lib')
    if ($spec.no_default) { $argv += '--no-default-features' }
    $argv += @('--no-run', '--message-format=json')
    $logName = "cargo-$($spec.id).jsonl"; $errName = "cargo-$($spec.id).stderr.txt"
    Write-Host "Compiling build-only $($spec.id)"
    $commands.Add((Invoke-Recorded $Cargo $argv (Join-Path $OutputDirectory $logName) (Join-Path $OutputDirectory $errName)))
    $evidence.Add($logName); $evidence.Add($errName)
    $messages = @([IO.File]::ReadLines((Join-Path $OutputDirectory $logName), [Text.UTF8Encoding]::new($false, $true)) | ForEach-Object { Require (-not [string]::IsNullOrWhiteSpace($_)) 'Blank Cargo JSON record'; ConvertFrom-Json -InputObject $_ -AsHashtable -Depth 100 })
    $finished = @($messages | Where-Object { $_.reason -ceq 'build-finished' })
    Require ($finished.Count -eq 1 -and $finished[0].success -is [bool] -and $finished[0].success -and $messages[-1].reason -ceq 'build-finished') 'Cargo did not finish successfully exactly once'
    $matches = @($messages | Where-Object { $_.reason -ceq 'compiler-artifact' -and $_.target.name -ceq $spec.target_name -and @($_.target.kind).Count -eq 1 -and $_.target.kind[0] -ceq 'lib' -and $_.profile.test -eq $true -and -not [string]::IsNullOrEmpty($_.executable) })
    Require ($matches.Count -eq 1) 'Expected a unique actual Cargo libtest primary'
    $primary = $matches[0]
    Require ([IO.Path]::GetFullPath($primary.manifest_path) -ieq (Join-Path $RepositoryRoot "crates/$($spec.package)/Cargo.toml")) 'Primary manifest mismatch'
    Require ($primary.fresh -is [bool]) 'Cargo fresh must be its actual boolean'
    Same-Strings $primary.features $spec.features
    $profile = $primary.profile
    foreach ($key in @('test', 'debug_assertions', 'overflow_checks')) { Require ($profile[$key] -is [bool] -and $profile[$key]) 'Unexpected Cargo test profile boolean' }
    Require ($profile.opt_level -is [string] -and $profile.opt_level -ceq '0' -and ($profile.debuginfo -is [int] -or $profile.debuginfo -is [long]) -and $profile.debuginfo -eq 0) 'Unexpected Cargo optimization/debug profile'
    $exe = [IO.Path]::GetFullPath($primary.executable)
    Assert-Under $exe $TargetRoot
    Require ([IO.Path]::GetDirectoryName($exe) -ieq (Join-Path $TargetRoot 'debug/deps')) 'Libtest is not in the ordinary debug deps directory'
    $pattern = '^' + [regex]::Escape($spec.target_name) + '-([0-9a-f]{16})\.exe$'
    $match = [regex]::Match([IO.Path]::GetFileName($exe), $pattern)
    Require $match.Success 'Unexpected libtest primary basename'
    Require (@($primary.filenames | Where-Object { [IO.Path]::GetFullPath($_) -ieq $exe }).Count -eq 1) 'Primary is not its unique Cargo executable filename'
    Assert-Pe64 $exe
    $origin = "target/debug/.fingerprint/$($spec.package)-$($match.Groups[1].Value)/test-lib-$($spec.target_name).json"
    $fpPath = Join-Path $RepositoryRoot $origin
    $fingerprint = Read-Json $fpPath
    Same-Strings (ConvertFrom-Json -InputObject $fingerprint.features -NoEnumerate) $spec.features
    Require (@($fingerprint.rustflags).Count -eq 0) 'Non-default Rust flags are forbidden'
    $fpName = "fingerprint-$($spec.id).json"
    $fp = Copy-Verified $fpPath (Join-Path $OutputDirectory $fpName)
    $evidence.Add($fpName)
    $basename = "$($spec.id).exe"
    $copy = Copy-Verified $exe (Join-Path $OutputDirectory $basename)
    Assert-Pe64 (Join-Path $OutputDirectory $basename)
    $artifacts.Add([ordered]@{ id = $spec.id; basename = $basename; sha256 = $copy.sha256; bytes = $copy.bytes; target = $TargetTriple; features = @($primary.features); profile = $profile; cargo_primary = $primary; cargo_log = @{ basename = $logName; sha256 = File-Sha (Join-Path $OutputDirectory $logName) }; fingerprint = @{ basename = $fpName; sha256 = $fp.sha256 }; fingerprint_origin = $origin; fresh = $primary.fresh; read_prepost_stat_stable = $true; stat = $copy })
}
Assert-Source
Require ((ConvertTo-Json -InputObject $assetsBefore -Depth 80 -Compress) -ceq (ConvertTo-Json -InputObject (Assets-Manifest) -Depth 80 -Compress)) 'Dashboard embedding assets changed during Cargo builds'
$evidenceFiles = [ordered]@{}
foreach ($relative in $evidence | Sort-Object) { $evidenceFiles[$relative] = File-Sha (Join-Path $OutputDirectory $relative) }
$allowlist = @($evidenceFiles.Keys) + @($artifacts | ForEach-Object { $_.basename })
foreach ($file in Get-ChildItem -LiteralPath $OutputDirectory -Force -Recurse -File) {
    $relative = [IO.Path]::GetRelativePath($OutputDirectory, $file.FullName).Replace('\', '/')
    Require ($allowlist -ccontains $relative) 'Unexpected artifact file'
    Assert-NormalPath $file.FullName $true
}
Write-Json (Join-Path $OutputDirectory 'manifest.json') ([ordered]@{ schema = 'duduclaw-native18-build-v1'; scope = $Scope; source = @{ head = $ProductHead; tree = $ProductTree }; target = $TargetTriple; source_files = $Inventory.source_files; cases = $Inventory.cases; artifacts = $artifacts.ToArray(); evidence_files = $evidenceFiles; workflow = @{ head = $WorkflowHead; run_id = $env:GITHUB_RUN_ID; run_attempt = $env:GITHUB_RUN_ATTEMPT; basename = 'workflow.yml'; sha256 = $evidenceFiles['workflow.yml']; collector = @{ basename = 'collector.ps1'; sha256 = $evidenceFiles['collector.ps1'] }; cases = @{ basename = 'cases.json'; sha256 = $evidenceFiles['cases.json'] } }; cargo_lock_sha256 = $LockSha; build_inputs = $Inventory.inputs; source_copy_stat = $sourceProofs; commands = $commands.ToArray(); current_source_Cargo_validated_libtests = $true; fresh_true_allowed_not_claimed_new_compilation = $true; build_only = $true; shipping_binary = $false; candidate_executed = $false; release_approved = $false; GUI_or_provider_acceptance = $false })
Write-Host 'Validated three current-source Cargo libtests; no generated executable was run.'
