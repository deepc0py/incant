#requires -Version 7.4
#requires -Modules Pester

BeforeAll {
    . (Join-Path $PSScriptRoot '..\install.ps1')
}

Describe 'install.ps1' {
    BeforeEach {
        $script:OriginalHome = $env:HOME
        $env:HOME = Join-Path $TestDrive 'home'
        New-Item -ItemType Directory -Path $env:HOME -Force | Out-Null

        $script:InstallDir = Join-Path $env:HOME 'bin'
        $script:ConfigDir = Join-Path $env:HOME 'config'
        $script:ProfilePath = Join-Path $env:HOME 'profile.ps1'
        $script:ExistingPath = Join-Path $env:HOME 'existing-bin'
        $script:FakeUserPath = $script:ExistingPath

        $fixtureRoot = Join-Path $TestDrive 'fixture'
        $fixturePackage = Join-Path $fixtureRoot 'incant-1.2.3-x86_64-pc-windows-msvc'
        New-Item -ItemType Directory -Path $fixturePackage -Force | Out-Null
        Set-Content -LiteralPath (Join-Path $fixturePackage 'incant.exe') -Value 'fixture executable'
        Set-Content -LiteralPath (Join-Path $fixturePackage 'config.example.toml') -Value 'backend = "test"'

        $script:AssetName = 'incant-1.2.3-x86_64-pc-windows-msvc.zip'
        $script:ArchivePath = Join-Path $TestDrive $script:AssetName
        Compress-Archive -Path $fixturePackage -DestinationPath $script:ArchivePath
        $hash = (Get-FileHash -LiteralPath $script:ArchivePath -Algorithm SHA256).Hash
        $script:ChecksumPath = Join-Path $TestDrive 'SHA256SUMS'
        Set-Content -LiteralPath $script:ChecksumPath -Value "$hash  $($script:AssetName)"

        Mock Get-IncantWindowsTarget { 'x86_64-pc-windows-msvc' }
        Mock Get-IncantUserPath { $script:FakeUserPath }
        Mock Set-IncantUserPath { param($Value) $script:FakeUserPath = $Value }
        Mock Invoke-IncantDownload {
            param($Uri, $Destination)
            if ([IO.Path]::GetFileName($Destination) -eq 'SHA256SUMS') {
                Copy-Item -LiteralPath $script:ChecksumPath -Destination $Destination
            }
            else {
                Copy-Item -LiteralPath $script:ArchivePath -Destination $Destination
            }
        }
    }

    AfterEach {
        $env:HOME = $script:OriginalHome
    }

    It 'normalizes a leading v for the tag URL but not the archive basename' {
        Invoke-IncantInstaller -Version 'v1.2.3' -InstallDir $script:InstallDir `
            -ConfigDir $script:ConfigDir -ProfilePath $script:ProfilePath

        Should -Invoke Invoke-IncantDownload -Times 1 -Exactly -ParameterFilter {
            $Uri.AbsoluteUri -eq 'https://github.com/deepc0py/incant/releases/download/v1.2.3/incant-1.2.3-x86_64-pc-windows-msvc.zip'
        }
        Should -Invoke Invoke-IncantDownload -Times 1 -Exactly -ParameterFilter {
            $Uri.AbsoluteUri -eq 'https://github.com/deepc0py/incant/releases/download/v1.2.3/SHA256SUMS'
        }
    }

    It 'selects the checksum for the exact zip basename' {
        $actualHash = (Get-FileHash -LiteralPath $script:ArchivePath -Algorithm SHA256).Hash
        Set-Content -LiteralPath $script:ChecksumPath -Value @(
            \"$('0' * 64)  incant-1.2.3-aarch64-pc-windows-msvc.zip\"
            \"$actualHash  $($script:AssetName)\"
        )

        {
            Assert-IncantArchiveChecksum -ArchivePath $script:ArchivePath `
                -ChecksumPath $script:ChecksumPath -AssetName $script:AssetName
        } | Should -Not -Throw

        Set-Content -LiteralPath $script:ChecksumPath `
            -Value \"$('0' * 64)  incant-1.2.3-aarch64-pc-windows-msvc.zip\"
        {
            Assert-IncantArchiveChecksum -ArchivePath $script:ArchivePath `
                -ChecksumPath $script:ChecksumPath -AssetName $script:AssetName
        } | Should -Throw \"*checksum for $($script:AssetName) was not found*\"
    }

    It 'is idempotent and preserves an existing config' {
        Invoke-IncantInstaller -Version '1.2.3' -InstallDir $script:InstallDir `
            -ConfigDir $script:ConfigDir -ProfilePath $script:ProfilePath
        Set-Content -LiteralPath (Join-Path $script:ConfigDir 'config.toml') -Value 'user setting'

        Invoke-IncantInstaller -Version '1.2.3' -InstallDir $script:InstallDir `
            -ConfigDir $script:ConfigDir -ProfilePath $script:ProfilePath

        (Get-Content -LiteralPath (Join-Path $script:ConfigDir 'config.toml') -Raw).Trim() |
            Should -Be 'user setting'
        $script:FakeUserPath.Split(';') |
            Where-Object { (Get-NormalizedPathEntry $_) -ieq (Get-NormalizedPathEntry $script:InstallDir) } |
            Should -HaveCount 1
        $profile = Get-Content -LiteralPath $script:ProfilePath -Raw
        ([regex]::Matches($profile, [regex]::Escape($script:ProfileStartMarker))).Count | Should -Be 1
        ([regex]::Matches($profile, [regex]::Escape($script:ProfileEndMarker))).Count | Should -Be 1
    }

    It 'makes no installation changes when checksum verification fails' {
        Set-Content -LiteralPath $script:ChecksumPath -Value "$('0' * 64)  $($script:AssetName)"

        {
            Invoke-IncantInstaller -Version '1.2.3' -InstallDir $script:InstallDir `
                -ConfigDir $script:ConfigDir -ProfilePath $script:ProfilePath
        } | Should -Throw '*SHA-256 verification failed*'

        Test-Path -LiteralPath (Join-Path $script:InstallDir 'incant.exe') | Should -BeFalse
        Test-Path -LiteralPath (Join-Path $script:ConfigDir 'config.toml') | Should -BeFalse
        Test-Path -LiteralPath $script:ProfilePath | Should -BeFalse
        $script:FakeUserPath | Should -Be $script:ExistingPath
    }

    It 'installs one clearly delimited Ctrl+K profile block' {
        Invoke-IncantInstaller -Version '1.2.3' -InstallDir $script:InstallDir `
            -ConfigDir $script:ConfigDir -ProfilePath $script:ProfilePath
        Invoke-IncantInstaller -Version '1.2.3' -InstallDir $script:InstallDir `
            -ConfigDir $script:ConfigDir -ProfilePath $script:ProfilePath

        $profile = Get-Content -LiteralPath $script:ProfilePath -Raw
        $profile | Should -Match "Set-PSReadLineKeyHandler -Chord 'Ctrl\+k'"
        $profile | Should -Match [regex]::Escape('[Microsoft.PowerShell.PSConsoleReadLine]::RevertLine()')
        $profile | Should -Match [regex]::Escape('[Microsoft.PowerShell.PSConsoleReadLine]::Insert($result)')
        $profile | Should -Not -Match 'AcceptLine'
        ([regex]::Matches($profile, [regex]::Escape($script:ProfileStartMarker))).Count | Should -Be 1
    }

    It 'does not replace the PSReadLine buffer after a failed invocation' {
        $block = Get-IncantProfileBlock
        $functionOnly = $block.Substring(0, $block.IndexOf("`n`nif (Get-Module", [StringComparison]::Ordinal))
        Invoke-Expression $functionOnly
        $script:BufferWasReplaced = $false

        _IncantInvokePSReadLine `
            -ReadBuffer { [pscustomobject]@{ Line = 'original'; Cursor = 4 } } `
            -InvokeCommand { throw 'daemon unavailable' } `
            -ReplaceBuffer { $script:BufferWasReplaced = $true } 2>$null

        $script:BufferWasReplaced | Should -BeFalse
    }

    It 'uninstalls owned state while preserving config and unrelated files' {
        Set-Content -LiteralPath $script:ProfilePath -Value '# user profile setting'
        Invoke-IncantInstaller -Version '1.2.3' -InstallDir $script:InstallDir `
            -ConfigDir $script:ConfigDir -ProfilePath $script:ProfilePath
        Set-Content -LiteralPath (Join-Path $script:InstallDir 'keep.txt') -Value 'not owned by Incant'

        Invoke-IncantInstaller -Uninstall -InstallDir $script:InstallDir `
            -ConfigDir $script:ConfigDir -ProfilePath $script:ProfilePath

        Test-Path -LiteralPath (Join-Path $script:InstallDir 'incant.exe') | Should -BeFalse
        Test-Path -LiteralPath (Join-Path $script:InstallDir 'keep.txt') | Should -BeTrue
        Test-Path -LiteralPath (Join-Path $script:ConfigDir 'config.toml') | Should -BeTrue
        (Get-Content -LiteralPath $script:ProfilePath -Raw) | Should -Not -Match 'incant PSReadLine integration'
        $script:FakeUserPath | Should -Be $script:ExistingPath
    }

    It 'removes config only when explicitly requested' {
        Invoke-IncantInstaller -Version '1.2.3' -InstallDir $script:InstallDir `
            -ConfigDir $script:ConfigDir -ProfilePath $script:ProfilePath

        Invoke-IncantInstaller -Uninstall -RemoveConfig -InstallDir $script:InstallDir `
            -ConfigDir $script:ConfigDir -ProfilePath $script:ProfilePath

        Test-Path -LiteralPath (Join-Path $script:ConfigDir 'config.toml') | Should -BeFalse
    }
}
