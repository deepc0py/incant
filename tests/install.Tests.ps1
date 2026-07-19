#requires -Version 7.4
#requires -Modules Pester

BeforeAll {
    . (Join-Path $PSScriptRoot '..\install.ps1')
}

Describe 'install.ps1' {
    BeforeEach {
        $script:OriginalHome = $env:HOME
        $script:OriginalProcessPath = $env:Path
        $env:HOME = Join-Path $TestDrive 'home'
        Remove-Item -LiteralPath $env:HOME -Recurse -Force -ErrorAction SilentlyContinue
        New-Item -ItemType Directory -Path $env:HOME -Force | Out-Null

        $script:InstallDir = Join-Path $env:HOME 'bin'
        $script:ConfigDir = Join-Path $env:HOME 'config'
        $script:ProfilePath = Join-Path $env:HOME 'profile.ps1'
        $script:ExistingPath = Join-Path $env:HOME 'existing-bin'
        $script:FakeUserPath = $script:ExistingPath
        $env:Path = $script:ExistingPath

        $fixtureRoot = Join-Path $TestDrive 'fixture'
        $fixturePackage = Join-Path $fixtureRoot 'incant-1.2.3-x86_64-pc-windows-msvc'
        New-Item -ItemType Directory -Path $fixturePackage -Force | Out-Null
        Set-Content -LiteralPath (Join-Path $fixturePackage 'incant.exe') -Value 'fixture executable'
        Set-Content -LiteralPath (Join-Path $fixturePackage 'config.example.toml') -Value 'backend = "test"'

        $script:AssetName = 'incant-1.2.3-x86_64-pc-windows-msvc.zip'
        $script:ArchivePath = Join-Path $TestDrive $script:AssetName
        Compress-Archive -Path $fixturePackage -DestinationPath $script:ArchivePath -Force
        $hash = (Get-FileHash -LiteralPath $script:ArchivePath -Algorithm SHA256).Hash
        $script:ChecksumPath = Join-Path $TestDrive 'SHA256SUMS'
        Set-Content -LiteralPath $script:ChecksumPath -Value "$hash  $($script:AssetName)"

        Mock Get-IncantWindowsTarget { 'x86_64-pc-windows-msvc' }
        Mock Get-IncantUserPath { $script:FakeUserPath }
        Mock Set-IncantUserPath { param($Value) $script:FakeUserPath = $Value }
        Mock Invoke-IncantDaemonStop {
            [pscustomobject]@{ ExitCode = 0; Output = 'Daemon is not running' }
        }
        Mock Wait-IncantBinaryUnlocked {}
        Mock Write-Host {}
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
        $env:Path = $script:OriginalProcessPath
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
            "$('0' * 64)  incant-1.2.3-aarch64-pc-windows-msvc.zip"
            "$actualHash  $($script:AssetName)"
        )

        {
            Assert-IncantArchiveChecksum -ArchivePath $script:ArchivePath `
                -ChecksumPath $script:ChecksumPath -AssetName $script:AssetName
        } | Should -Not -Throw

        Set-Content -LiteralPath $script:ChecksumPath `
            -Value "$('0' * 64)  incant-1.2.3-aarch64-pc-windows-msvc.zip"
        {
            Assert-IncantArchiveChecksum -ArchivePath $script:ArchivePath `
                -ChecksumPath $script:ChecksumPath -AssetName $script:AssetName
        } | Should -Throw "*checksum for $($script:AssetName) was not found*"
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
        $env:Path.Split([IO.Path]::PathSeparator) |
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
        $profile | Should -Match ([regex]::Escape('[Microsoft.PowerShell.PSConsoleReadLine]::RevertLine()'))
        $profile | Should -Match ([regex]::Escape('[Microsoft.PowerShell.PSConsoleReadLine]::Insert($result)'))
        $profile | Should -Not -Match 'AcceptLine'
        $profile | Should -Not -Match '2>\$null'
        ([regex]::Matches($profile, [regex]::Escape($script:ProfileStartMarker))).Count | Should -Be 1
    }

    It 'does not replace the PSReadLine buffer after a failed invocation' {
        $block = Get-IncantProfileBlock
        $functionOnly = $block.Substring(0, $block.IndexOf("`n`nif (Get-Module", [StringComparison]::Ordinal))
        Invoke-Expression $functionOnly
        $script:BufferWasReplaced = $false
        $script:Diagnostic = $null

        _IncantInvokePSReadLine `
            -ReadBuffer { [pscustomobject]@{ Line = 'original'; Cursor = 4 } } `
            -InvokeCommand { throw 'daemon unavailable' } `
            -ReplaceBuffer { $script:BufferWasReplaced = $true } `
            -WriteDiagnostic { param($message) $script:Diagnostic = $message }

        $script:BufferWasReplaced | Should -BeFalse
        $script:Diagnostic | Should -Be 'incant: daemon unavailable'
    }

    It 'returns silently without replacing the buffer when incant is cancelled' {
        $block = Get-IncantProfileBlock
        $functionOnly = $block.Substring(0, $block.IndexOf("`n`nif (Get-Module", [StringComparison]::Ordinal))
        Invoke-Expression $functionOnly
        $script:BufferWasReplaced = $false
        $script:Diagnostic = $null

        _IncantInvokePSReadLine `
            -ReadBuffer { [pscustomobject]@{ Line = 'original'; Cursor = 4 } } `
            -InvokeCommand { [pscustomobject]@{ ExitCode = 0; Output = @() } } `
            -ReplaceBuffer { $script:BufferWasReplaced = $true } `
            -WriteDiagnostic { param($message) $script:Diagnostic = $message }

        $script:BufferWasReplaced | Should -BeFalse
        $script:Diagnostic | Should -BeNullOrEmpty
    }

    It 'stops the existing daemon and waits for release before upgrading' {
        Invoke-IncantInstaller -Version '1.2.3' -InstallDir $script:InstallDir `
            -ConfigDir $script:ConfigDir -ProfilePath $script:ProfilePath
        Mock Invoke-IncantDaemonStop {
            [pscustomobject]@{ ExitCode = 0; Output = 'Daemon stopped' }
        }

        Invoke-IncantInstaller -Version '1.2.3' -InstallDir $script:InstallDir `
            -ConfigDir $script:ConfigDir -ProfilePath $script:ProfilePath

        Should -Invoke Invoke-IncantDaemonStop -Times 1 -Exactly -ParameterFilter {
            $BinaryPath -eq (Join-Path $script:InstallDir 'incant.exe')
        }
        Should -Invoke Wait-IncantBinaryUnlocked -Times 1 -Exactly -ParameterFilter {
            $BinaryPath -eq (Join-Path $script:InstallDir 'incant.exe') -and
            $Operation -eq 'upgrade'
        }
        Should -Invoke Write-Host -Times 1 -Exactly -ParameterFilter {
            $Object -eq "Incant upgraded successfully: $(Join-Path $script:InstallDir 'incant.exe')"
        }
    }

    It 'tolerates only documented daemon stop responses' {
        $binary = Join-Path $script:InstallDir 'incant.exe'
        {
            Stop-IncantForBinaryChange -BinaryPath $binary -Operation 'upgrade'
        } | Should -Not -Throw

        Mock Invoke-IncantDaemonStop {
            [pscustomobject]@{ ExitCode = 0; Output = 'unknown state' }
        }
        {
            Stop-IncantForBinaryChange -BinaryPath $binary -Operation 'upgrade'
        } | Should -Throw "*unexpected daemon stop response 'unknown state'*"
    }

    It 'fails with a precise diagnostic when the installed binary remains locked' {
        $binary = Join-Path $script:InstallDir 'incant.exe'
        Mock Wait-IncantBinaryUnlocked {
            throw "Cannot upgrade Incant: '$BinaryPath' is still locked after stopping the daemon."
        }

        {
            Stop-IncantForBinaryChange -BinaryPath $binary -Operation 'upgrade'
        } | Should -Throw "*is still locked after stopping the daemon*"
    }

    It 'reports installation and new-shell guidance' {
        Invoke-IncantInstaller -Version '1.2.3' -InstallDir $script:InstallDir `
            -ConfigDir $script:ConfigDir -ProfilePath $script:ProfilePath

        Should -Invoke Write-Host -Times 1 -Exactly -ParameterFilter {
            $Object -eq "Incant installed successfully: $(Join-Path $script:InstallDir 'incant.exe')"
        }
        Should -Invoke Write-Host -Times 1 -Exactly -ParameterFilter {
            $Object -eq 'Open a new PowerShell session to load the Ctrl+K integration and persistent PATH.'
        }
    }

    It 'stops the running daemon and waits for release before uninstalling the binary' {
        $binary = Join-Path $script:InstallDir 'incant.exe'
        New-Item -ItemType Directory -Path $script:InstallDir -Force | Out-Null
        Set-Content -LiteralPath $binary -Value 'installed executable'
        $script:DaemonStopped = $false
        Mock Invoke-IncantDaemonStop {
            $script:DaemonStopped = $true
            [pscustomobject]@{ ExitCode = 0; Output = 'Daemon stopped' }
        }
        Mock Wait-IncantBinaryUnlocked {
            $script:DaemonStopped | Should -BeTrue
            Test-Path -LiteralPath $BinaryPath | Should -BeTrue
        }

        Invoke-IncantInstaller -Uninstall -InstallDir $script:InstallDir `
            -ConfigDir $script:ConfigDir -ProfilePath $script:ProfilePath

        Should -Invoke Invoke-IncantDaemonStop -Times 1 -Exactly -ParameterFilter {
            $BinaryPath -eq $binary
        }
        Should -Invoke Wait-IncantBinaryUnlocked -Times 1 -Exactly -ParameterFilter {
            $BinaryPath -eq $binary -and $Operation -eq 'uninstall'
        }
        Test-Path -LiteralPath $binary | Should -BeFalse
        Should -Invoke Write-Host -Times 1 -Exactly -ParameterFilter {
            $Object -eq 'Incant uninstalled successfully.'
        }
    }

    It 'fails precisely without reporting success when the binary cannot be deleted' {
        $binary = Join-Path $script:InstallDir 'incant.exe'
        New-Item -ItemType Directory -Path $script:InstallDir -Force | Out-Null
        Set-Content -LiteralPath $binary -Value 'installed executable'
        Mock Remove-Item {
            throw [IO.IOException]::new('The process cannot access the file because it is in use.')
        } -ParameterFilter { $LiteralPath -eq $binary }

        {
            Invoke-IncantInstaller -Uninstall -InstallDir $script:InstallDir `
                -ConfigDir $script:ConfigDir -ProfilePath $script:ProfilePath
        } | Should -Throw "*Cannot uninstall Incant: failed to remove '$binary':*file because it is in use*"

        Test-Path -LiteralPath $binary | Should -BeTrue
        Should -Invoke Write-Host -Times 0 -Exactly -ParameterFilter {
            $Object -eq 'Incant uninstalled successfully.'
        }
    }

    It 'is idempotent when the installed binary is already absent' {
        {
            Invoke-IncantInstaller -Uninstall -InstallDir $script:InstallDir `
                -ConfigDir $script:ConfigDir -ProfilePath $script:ProfilePath
        } | Should -Not -Throw

        Should -Invoke Invoke-IncantDaemonStop -Times 0 -Exactly
        Should -Invoke Wait-IncantBinaryUnlocked -Times 0 -Exactly
        Test-Path -LiteralPath (Join-Path $script:InstallDir 'incant.exe') | Should -BeFalse
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

    It 'fails without reporting success when requested config removal leaves the file behind' {
        $configPath = Join-Path $script:ConfigDir 'config.toml'
        New-Item -ItemType Directory -Path $script:ConfigDir -Force | Out-Null
        Set-Content -LiteralPath $configPath -Value 'user setting'
        Mock Remove-Item {} -ParameterFilter { $LiteralPath -eq $configPath }

        {
            Invoke-IncantInstaller -Uninstall -RemoveConfig -InstallDir $script:InstallDir `
                -ConfigDir $script:ConfigDir -ProfilePath $script:ProfilePath
        } | Should -Throw "*Cannot uninstall Incant: '$configPath' still exists after removal.*"

        Test-Path -LiteralPath $configPath | Should -BeTrue
        Should -Invoke Write-Host -Times 0 -Exactly -ParameterFilter {
            $Object -eq 'Incant uninstalled successfully.'
        }
    }

    It 'removes config only when explicitly requested' {
        Invoke-IncantInstaller -Version '1.2.3' -InstallDir $script:InstallDir `
            -ConfigDir $script:ConfigDir -ProfilePath $script:ProfilePath

        Invoke-IncantInstaller -Uninstall -RemoveConfig -InstallDir $script:InstallDir `
            -ConfigDir $script:ConfigDir -ProfilePath $script:ProfilePath

        Test-Path -LiteralPath (Join-Path $script:ConfigDir 'config.toml') | Should -BeFalse
    }
}
