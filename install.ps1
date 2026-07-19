#requires -Version 7.4

[CmdletBinding(DefaultParameterSetName = 'Release')]
param(
    [Parameter(Mandatory, ParameterSetName = 'Source')]
    [switch] $FromSource,

    [Parameter(Mandatory, ParameterSetName = 'Uninstall')]
    [switch] $Uninstall,

    [Parameter(ParameterSetName = 'Release')]
    [ValidateNotNullOrEmpty()]
    [string] $Version = 'latest',

    [Parameter(ParameterSetName = 'Release')]
    [ValidateNotNullOrEmpty()]
    [string] $ReleaseBaseUri = 'https://github.com/deepc0py/incant/releases/download',

    [Parameter(ParameterSetName = 'Source')]
    [ValidateSet('Debug', 'Release')]
    [string] $BuildProfile = 'Debug',

    [Parameter(ParameterSetName = 'Source')]
    [ValidateNotNullOrEmpty()]
    [string] $SourcePath = $PSScriptRoot,

    [Parameter(ParameterSetName = 'Uninstall')]
    [switch] $RemoveConfig,

    [ValidateNotNullOrEmpty()]
    [string] $InstallDir = (Join-Path ([Environment]::GetFolderPath('LocalApplicationData')) 'Incant\bin'),

    [ValidateNotNullOrEmpty()]
    [string] $ConfigDir = (Join-Path ([Environment]::GetFolderPath('LocalApplicationData')) 'incant'),

    [ValidateNotNullOrEmpty()]
    [string] $ProfilePath = $PROFILE.CurrentUserAllHosts
)

Set-StrictMode -Version 3.0
$ErrorActionPreference = 'Stop'

$script:ProfileStartMarker = '# >>> incant PSReadLine integration >>>'
$script:ProfileEndMarker = '# <<< incant PSReadLine integration <<<'

function Get-IncantUserPath {
    [Environment]::GetEnvironmentVariable('Path', 'User')
}

function Set-IncantUserPath {
    param([AllowEmptyString()][string] $Value)

    [Environment]::SetEnvironmentVariable('Path', $Value, 'User')
}

function Get-NormalizedPathEntry {
    param([Parameter(Mandatory)][string] $Path)

    [Environment]::ExpandEnvironmentVariables($Path.Trim().Trim('"')).TrimEnd(
        [IO.Path]::DirectorySeparatorChar,
        [IO.Path]::AltDirectorySeparatorChar
    )
}

function Add-IncantUserPathEntry {
    param([Parameter(Mandatory)][string] $Path)

    $normalized = Get-NormalizedPathEntry $Path
    $entries = @(
        (Get-IncantUserPath) -split ';' |
            Where-Object { -not [string]::IsNullOrWhiteSpace($_) }
    )
    if (-not ($entries | Where-Object { (Get-NormalizedPathEntry $_) -ieq $normalized })) {
        Set-IncantUserPath (($entries + $normalized) -join ';')
    }

    $processEntries = @(
        $env:Path -split [IO.Path]::PathSeparator |
            Where-Object { -not [string]::IsNullOrWhiteSpace($_) }
    )
    if (-not ($processEntries | Where-Object { (Get-NormalizedPathEntry $_) -ieq $normalized })) {
        $env:Path = ($processEntries + $normalized) -join [IO.Path]::PathSeparator
    }
}

function Remove-IncantUserPathEntry {
    param([Parameter(Mandatory)][string] $Path)

    $normalized = Get-NormalizedPathEntry $Path
    $entries = @(
        (Get-IncantUserPath) -split ';' |
            Where-Object { -not [string]::IsNullOrWhiteSpace($_) }
    )
    $remainingEntries = @(
        $entries |
            Where-Object { (Get-NormalizedPathEntry $_) -ine $normalized }
    )
    if ($remainingEntries.Count -ne $entries.Count) {
        Set-IncantUserPath ($remainingEntries -join ';')
    }

    $processEntries = @(
        $env:Path -split [IO.Path]::PathSeparator |
            Where-Object { -not [string]::IsNullOrWhiteSpace($_) }
    )
    $remainingProcessEntries = @(
        $processEntries |
            Where-Object { (Get-NormalizedPathEntry $_) -ine $normalized }
    )
    if ($remainingProcessEntries.Count -ne $processEntries.Count) {
        $env:Path = $remainingProcessEntries -join [IO.Path]::PathSeparator
    }
}

function Get-IncantProfileBlock {
    @'
# >>> incant PSReadLine integration >>>
function _IncantInvokePSReadLine {
    [CmdletBinding()]
    param(
        [scriptblock] $ReadBuffer = {
            $line = $null
            $cursor = 0
            [Microsoft.PowerShell.PSConsoleReadLine]::GetBufferState([ref] $line, [ref] $cursor)
            [pscustomobject]@{ Line = $line; Cursor = $cursor }
        },
        [scriptblock] $InvokeCommand = {
            param([string] $line)
            $stdout = @(& incant -- $line)
            [pscustomobject]@{ ExitCode = $LASTEXITCODE; Output = $stdout }
        },
        [scriptblock] $ReplaceBuffer = {
            param([string] $result)
            [Microsoft.PowerShell.PSConsoleReadLine]::RevertLine()
            [Microsoft.PowerShell.PSConsoleReadLine]::Insert($result)
        },
        [scriptblock] $WriteDiagnostic = {
            param([string] $message)
            [Console]::Error.WriteLine($message)
        }
    )

    $buffer = & $ReadBuffer
    try {
        $response = & $InvokeCommand $buffer.Line
        if ($response.ExitCode -ne 0) {
            throw "incant exited with status $($response.ExitCode)"
        }
        $result = @($response.Output) -join [Environment]::NewLine
        if ([string]::IsNullOrWhiteSpace($result)) {
            return
        }
    }
    catch {
        & $WriteDiagnostic "incant: $($_.Exception.Message)"
        return
    }

    & $ReplaceBuffer $result
}

if (Get-Module -ListAvailable -Name PSReadLine) {
    Set-PSReadLineKeyHandler -Chord 'Ctrl+k' -ScriptBlock { _IncantInvokePSReadLine }
}
# <<< incant PSReadLine integration <<<
'@
}

function Set-IncantProfileBlock {
    param([Parameter(Mandatory)][string] $Path)

    $parent = Split-Path -Parent $Path
    if (-not [string]::IsNullOrEmpty($parent)) {
        New-Item -ItemType Directory -Path $parent -Force | Out-Null
    }

    $content = if (Test-Path -LiteralPath $Path) {
        Get-Content -LiteralPath $Path -Raw
    }
    else {
        ''
    }
    $block = Get-IncantProfileBlock
    $pattern = '(?ms)^' + [regex]::Escape($script:ProfileStartMarker) +
        '\r?\n.*?^' + [regex]::Escape($script:ProfileEndMarker)

    if ([regex]::IsMatch($content, $pattern)) {
        $updated = [regex]::Replace($content, $pattern, [Text.RegularExpressions.MatchEvaluator] { param($match) $block })
    }
    else {
        $separator = if ($content.Length -eq 0 -or $content.EndsWith("`n")) { '' } else { [Environment]::NewLine }
        $updated = $content + $separator + $block + [Environment]::NewLine
    }

    Set-Content -LiteralPath $Path -Value $updated -NoNewline -Encoding utf8
}

function Remove-IncantProfileBlock {
    param([Parameter(Mandatory)][string] $Path)

    if (-not (Test-Path -LiteralPath $Path)) {
        return
    }

    $content = Get-Content -LiteralPath $Path -Raw
    $pattern = '(?ms)^' + [regex]::Escape($script:ProfileStartMarker) +
        '\r?\n.*?^' + [regex]::Escape($script:ProfileEndMarker) + '(?:\r?\n)?'
    $updated = [regex]::Replace($content, $pattern, '')
    if ($updated -ne $content) {
        Set-Content -LiteralPath $Path -Value $updated -NoNewline -Encoding utf8
    }
}

function Invoke-IncantDownload {
    param(
        [Parameter(Mandatory)][uri] $Uri,
        [Parameter(Mandatory)][string] $Destination
    )

    Invoke-WebRequest -Uri $Uri -OutFile $Destination
}

function ConvertFrom-IncantReleaseTag {
    param([Parameter(Mandatory)][string] $Tag)

    $version = $Tag.Trim()
    if ($version.StartsWith('v', [StringComparison]::OrdinalIgnoreCase)) {
        $version = $version.Substring(1)
    }
    if ([string]::IsNullOrWhiteSpace($version)) {
        throw "Invalid Incant release tag: '$Tag'."
    }
    $version
}

function Resolve-IncantReleaseVersion {
    param([Parameter(Mandatory)][string] $RequestedVersion)

    if ($RequestedVersion -ne 'latest') {
        return ConvertFrom-IncantReleaseTag $RequestedVersion
    }

    $release = Invoke-RestMethod -Uri 'https://api.github.com/repos/deepc0py/incant/releases/latest'
    if ([string]::IsNullOrWhiteSpace($release.tag_name)) {
        throw 'The latest GitHub release did not include a tag name.'
    }
    ConvertFrom-IncantReleaseTag ([string] $release.tag_name)
}

function Get-IncantWindowsTarget {
    $architecture = [Runtime.InteropServices.RuntimeInformation]::OSArchitecture.ToString()
    switch ($architecture) {
        'X64' { 'x86_64-pc-windows-msvc' }
        'Arm64' { 'aarch64-pc-windows-msvc' }
        default { throw "Unsupported Windows architecture: $architecture" }
    }
}

function Assert-IncantArchiveChecksum {
    param(
        [Parameter(Mandatory)][string] $ArchivePath,
        [Parameter(Mandatory)][string] $ChecksumPath,
        [Parameter(Mandatory)][string] $AssetName
    )

    $pattern = '^(?<hash>[A-Fa-f0-9]{64})\s+\*?(?:\./)?' + [regex]::Escape($AssetName) + '$'
    $checksumLine = Get-Content -LiteralPath $ChecksumPath |
        Where-Object { $_ -match $pattern } |
        Select-Object -First 1
    if ($null -eq $checksumLine -or $checksumLine -notmatch $pattern) {
        throw "SHA-256 checksum for $AssetName was not found."
    }

    $expected = $Matches.hash
    $actual = (Get-FileHash -LiteralPath $ArchivePath -Algorithm SHA256).Hash
    if ($actual -ine $expected) {
        throw "SHA-256 verification failed for $AssetName."
    }
}

function Invoke-IncantDaemonStop {
    param([Parameter(Mandatory)][string] $BinaryPath)

    $output = @(& $BinaryPath daemon stop 2>&1)
    [pscustomobject]@{
        ExitCode = $LASTEXITCODE
        Output = (@($output | ForEach-Object { $_.ToString() }) -join [Environment]::NewLine).Trim()
    }
}

function Wait-IncantBinaryUnlocked {
    param(
        [Parameter(Mandatory)][string] $BinaryPath,
        [Parameter(Mandatory)][ValidateSet('upgrade', 'uninstall')][string] $Operation,
        [int] $TimeoutMilliseconds = 5000
    )

    $deadline = [DateTime]::UtcNow.AddMilliseconds($TimeoutMilliseconds)
    do {
        try {
            $stream = [IO.File]::Open(
                $BinaryPath,
                [IO.FileMode]::Open,
                [IO.FileAccess]::ReadWrite,
                [IO.FileShare]::None
            )
            $stream.Dispose()
            return
        }
        catch [IO.IOException] {
            if ([DateTime]::UtcNow -ge $deadline) {
                throw "Cannot $Operation Incant: '$BinaryPath' is still locked after stopping the daemon."
            }
            Start-Sleep -Milliseconds 100
        }
    } while ($true)
}

function Stop-IncantForBinaryChange {
    param(
        [Parameter(Mandatory)][string] $BinaryPath,
        [Parameter(Mandatory)][ValidateSet('upgrade', 'uninstall')][string] $Operation
    )

    $result = Invoke-IncantDaemonStop $BinaryPath
    if ($result.ExitCode -ne 0) {
        $detail = if ([string]::IsNullOrWhiteSpace($result.Output)) { '' } else { ": $($result.Output)" }
        throw "Cannot $Operation Incant: 'incant.exe daemon stop' failed with status $($result.ExitCode)$detail"
    }
    if ($result.Output -notin @('Daemon stopped', 'Daemon is not running')) {
        throw "Cannot $Operation Incant: unexpected daemon stop response '$($result.Output)'."
    }

    Wait-IncantBinaryUnlocked -BinaryPath $BinaryPath -Operation $Operation
}

function Install-IncantFiles {
    param(
        [Parameter(Mandatory)][string] $BinaryPath,
        [Parameter(Mandatory)][string] $ConfigTemplatePath,
        [Parameter(Mandatory)][string] $Destination,
        [Parameter(Mandatory)][string] $ConfigurationDirectory,
        [Parameter(Mandatory)][string] $PowerShellProfile
    )

    $installedBinary = Join-Path $Destination 'incant.exe'
    $isUpgrade = Test-Path -LiteralPath $installedBinary
    if ($isUpgrade) {
        Stop-IncantForBinaryChange -BinaryPath $installedBinary -Operation 'upgrade'
    }

    New-Item -ItemType Directory -Path $Destination -Force | Out-Null
    Copy-Item -LiteralPath $BinaryPath -Destination $installedBinary -Force

    $configPath = Join-Path $ConfigurationDirectory 'config.toml'
    if (-not (Test-Path -LiteralPath $configPath)) {
        New-Item -ItemType Directory -Path $ConfigurationDirectory -Force | Out-Null
        Copy-Item -LiteralPath $ConfigTemplatePath -Destination $configPath
    }

    Add-IncantUserPathEntry $Destination
    Set-IncantProfileBlock $PowerShellProfile

    $operation = if ($isUpgrade) { 'upgraded' } else { 'installed' }
    Write-Host "Incant $operation successfully: $installedBinary"
    Write-Host 'Open a new PowerShell session to load the Ctrl+K integration and persistent PATH.'
}

function Install-IncantRelease {
    param(
        [Parameter(Mandatory)][string] $RequestedVersion,
        [Parameter(Mandatory)][string] $BaseUri,
        [Parameter(Mandatory)][string] $Destination,
        [Parameter(Mandatory)][string] $ConfigurationDirectory,
        [Parameter(Mandatory)][string] $PowerShellProfile
    )

    $resolvedVersion = Resolve-IncantReleaseVersion $RequestedVersion
    $tag = "v$resolvedVersion"
    $assetName = "incant-$resolvedVersion-$(Get-IncantWindowsTarget).zip"
    $temporaryDirectory = Join-Path ([IO.Path]::GetTempPath()) "incant-$([guid]::NewGuid())"
    New-Item -ItemType Directory -Path $temporaryDirectory | Out-Null

    try {
        $archivePath = Join-Path $temporaryDirectory $assetName
        $checksumPath = Join-Path $temporaryDirectory 'SHA256SUMS'
        $releaseUri = $BaseUri.TrimEnd('/') + "/$tag"
        Invoke-IncantDownload -Uri "$releaseUri/$assetName" -Destination $archivePath
        Invoke-IncantDownload -Uri "$releaseUri/SHA256SUMS" -Destination $checksumPath
        Assert-IncantArchiveChecksum -ArchivePath $archivePath -ChecksumPath $checksumPath -AssetName $assetName

        $expandedPath = Join-Path $temporaryDirectory 'expanded'
        Expand-Archive -LiteralPath $archivePath -DestinationPath $expandedPath
        $binary = Get-ChildItem -LiteralPath $expandedPath -Filter 'incant.exe' -File -Recurse | Select-Object -First 1
        $config = Get-ChildItem -LiteralPath $expandedPath -Filter 'config.example.toml' -File -Recurse | Select-Object -First 1
        if ($null -eq $binary -or $null -eq $config) {
            throw "$assetName does not contain incant.exe and config.example.toml."
        }

        Install-IncantFiles -BinaryPath $binary.FullName -ConfigTemplatePath $config.FullName `
            -Destination $Destination -ConfigurationDirectory $ConfigurationDirectory `
            -PowerShellProfile $PowerShellProfile
    }
    finally {
        Remove-Item -LiteralPath $temporaryDirectory -Recurse -Force -ErrorAction SilentlyContinue
    }
}

function Install-IncantFromSource {
    param(
        [Parameter(Mandatory)][string] $RepositoryPath,
        [Parameter(Mandatory)][ValidateSet('Debug', 'Release')][string] $BuildProfile,
        [Parameter(Mandatory)][string] $Destination,
        [Parameter(Mandatory)][string] $ConfigurationDirectory,
        [Parameter(Mandatory)][string] $PowerShellProfile
    )

    $manifest = Join-Path $RepositoryPath 'Cargo.toml'
    $configTemplate = Join-Path $RepositoryPath 'config.example.toml'
    if (-not (Test-Path -LiteralPath $manifest) -or -not (Test-Path -LiteralPath $configTemplate)) {
        throw "$RepositoryPath is not an Incant source checkout."
    }

    $cargoArguments = @('build', '--locked', '--manifest-path', $manifest)
    if ($BuildProfile -eq 'Release') {
        $cargoArguments += '--release'
    }
    & cargo @cargoArguments
    if ($LASTEXITCODE -ne 0) {
        throw "cargo build failed with status $LASTEXITCODE."
    }

    $profileDirectory = if ($BuildProfile -eq 'Release') { 'release' } else { 'debug' }
    $binary = Join-Path $RepositoryPath "target/$profileDirectory/incant.exe"
    if (-not (Test-Path -LiteralPath $binary)) {
        throw "cargo did not produce $binary."
    }

    Install-IncantFiles -BinaryPath $binary -ConfigTemplatePath $configTemplate `
        -Destination $Destination -ConfigurationDirectory $ConfigurationDirectory `
        -PowerShellProfile $PowerShellProfile
}

function Uninstall-Incant {
    param(
        [Parameter(Mandatory)][string] $Destination,
        [Parameter(Mandatory)][string] $ConfigurationDirectory,
        [Parameter(Mandatory)][string] $PowerShellProfile,
        [switch] $DeleteConfig
    )

    $binary = Join-Path $Destination 'incant.exe'
    if (Test-Path -LiteralPath $binary) {
        Stop-IncantForBinaryChange -BinaryPath $binary -Operation 'uninstall'
        try {
            Remove-Item -LiteralPath $binary -Force -ErrorAction Stop
        }
        catch {
            throw "Cannot uninstall Incant: failed to remove '$binary': $($_.Exception.Message)"
        }
        if (Test-Path -LiteralPath $binary) {
            throw "Cannot uninstall Incant: '$binary' still exists after removal."
        }
    }
    if ((Test-Path -LiteralPath $Destination) -and
        @(Get-ChildItem -LiteralPath $Destination -Force).Count -eq 0) {
        Remove-Item -LiteralPath $Destination -Force
    }

    Remove-IncantUserPathEntry $Destination
    Remove-IncantProfileBlock $PowerShellProfile

    if ($DeleteConfig) {
        $configPath = Join-Path $ConfigurationDirectory 'config.toml'
        if (Test-Path -LiteralPath $configPath) {
            try {
                Remove-Item -LiteralPath $configPath -Force -ErrorAction Stop
            }
            catch {
                throw "Cannot uninstall Incant: failed to remove '$configPath': $($_.Exception.Message)"
            }
            if (Test-Path -LiteralPath $configPath) {
                throw "Cannot uninstall Incant: '$configPath' still exists after removal."
            }
        }
        if ((Test-Path -LiteralPath $ConfigurationDirectory) -and
            @(Get-ChildItem -LiteralPath $ConfigurationDirectory -Force).Count -eq 0) {
            Remove-Item -LiteralPath $ConfigurationDirectory -Force
        }
    }

    Write-Host 'Incant uninstalled successfully.'
}

function Invoke-IncantInstaller {
    [CmdletBinding(DefaultParameterSetName = 'Release')]
    param(
        [Parameter(Mandatory, ParameterSetName = 'Source')][switch] $FromSource,
        [Parameter(Mandatory, ParameterSetName = 'Uninstall')][switch] $Uninstall,
        [Parameter(ParameterSetName = 'Release')][string] $Version = 'latest',
        [Parameter(ParameterSetName = 'Release')][string] $ReleaseBaseUri = 'https://github.com/deepc0py/incant/releases/download',
        [Parameter(ParameterSetName = 'Source')][ValidateSet('Debug', 'Release')][string] $BuildProfile = 'Debug',
        [Parameter(ParameterSetName = 'Source')][string] $SourcePath = $PSScriptRoot,
        [Parameter(ParameterSetName = 'Uninstall')][switch] $RemoveConfig,
        [string] $InstallDir = (Join-Path ([Environment]::GetFolderPath('LocalApplicationData')) 'Incant\bin'),
        [string] $ConfigDir = (Join-Path ([Environment]::GetFolderPath('LocalApplicationData')) 'incant'),
        [string] $ProfilePath = $PROFILE.CurrentUserAllHosts
    )

    if ($PSVersionTable.PSVersion -lt [version] '7.4') {
        throw 'Incant requires PowerShell 7.4 or newer.'
    }

    switch ($PSCmdlet.ParameterSetName) {
        'Source' {
            Install-IncantFromSource -RepositoryPath $SourcePath -BuildProfile $BuildProfile `
                -Destination $InstallDir -ConfigurationDirectory $ConfigDir `
                -PowerShellProfile $ProfilePath
        }
        'Uninstall' {
            Uninstall-Incant -Destination $InstallDir -ConfigurationDirectory $ConfigDir `
                -PowerShellProfile $ProfilePath -DeleteConfig:$RemoveConfig
        }
        default {
            Install-IncantRelease -RequestedVersion $Version -BaseUri $ReleaseBaseUri `
                -Destination $InstallDir -ConfigurationDirectory $ConfigDir `
                -PowerShellProfile $ProfilePath
        }
    }
}

if ($MyInvocation.InvocationName -ne '.') {
    Invoke-IncantInstaller @PSBoundParameters
}
