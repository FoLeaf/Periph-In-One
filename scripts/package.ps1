param([string]$Executable = 'target/release/periph-in-one.exe')
$ErrorActionPreference = 'Stop'
Push-Location (Split-Path $PSScriptRoot -Parent)
try {
    $exe = (Resolve-Path -LiteralPath $Executable).Path
    $metadataJson = cargo metadata --locked --format-version 1 --filter-platform x86_64-pc-windows-msvc
    if ($LASTEXITCODE -ne 0) { throw 'Dependency metadata failed' }
    $metadata = $metadataJson | ConvertFrom-Json
    $project = $metadata.packages | Where-Object name -eq 'periph-in-one'
    $stage = Join-Path (Get-Location) ('dist/package-' + [guid]::NewGuid().ToString('N'))
    New-Item -ItemType Directory -Path "$stage/licenses/dependencies" -Force | Out-Null
    Copy-Item -LiteralPath $exe -Destination "$stage/periph-in-one.exe"
    Copy-Item -LiteralPath LICENSE -Destination $stage
    Copy-Item -LiteralPath licenses/Slint-Royalty-Free-2.0.md -Destination "$stage/licenses"
    # Ship the same README and every image it references, without local configuration.
    Copy-Item -LiteralPath README.md,contribution.md -Destination $stage
    New-Item -ItemType Directory -Path "$stage/docs" -Force | Out-Null
    Copy-Item -LiteralPath docs/ADAPTATION.md -Destination "$stage/docs"
    New-Item -ItemType Directory -Path "$stage/crates/periph-hal" -Force | Out-Null
    Copy-Item -LiteralPath crates/periph-hal/README.md -Destination "$stage/crates/periph-hal"
    New-Item -ItemType Directory -Path "$stage/.github" -Force | Out-Null
    Copy-Item -LiteralPath .github/CICD.md -Destination "$stage/.github"
    New-Item -ItemType Directory -Path "$stage/assets/readme" -Force | Out-Null
    Copy-Item -LiteralPath assets/slint-badge.svg -Destination "$stage/assets"
    Copy-Item -Path assets/readme/* -Destination "$stage/assets/readme"
    $notices = @('# Dependency notices', '', 'Slint is used under the Slint Royalty-free License 2.0.', '')
    foreach ($package in ($metadata.packages | Sort-Object name,version)) {
        if ($package.id -eq $project.id) { continue }
        $name = "$($package.name)-$($package.version)"
        $notices += "- ${name}: $($package.license)"
        $source = Split-Path $package.manifest_path -Parent
        $candidates = @(Get-ChildItem -LiteralPath $source -File)
        if (Test-Path -LiteralPath "$source/LICENSES") {
            $candidates += Get-ChildItem -LiteralPath "$source/LICENSES" -Recurse -File
        }
        foreach ($file in $candidates) {
            if ($file.Name -notmatch '^(LICENSE|LICENCE|COPYING|NOTICE)' -and $file.FullName -notlike "$source*LICENSES*") { continue }
            $relative = $file.FullName.Substring($source.Length + 1)
            $destination = Join-Path "$stage/licenses/dependencies/$name" $relative
            New-Item -ItemType Directory -Path (Split-Path $destination -Parent) -Force | Out-Null
            Copy-Item -LiteralPath $file.FullName -Destination $destination
        }
    }
    $notices | Set-Content -Encoding utf8 "$stage/licenses/DEPENDENCIES.md"
    $checksum = (Get-FileHash -LiteralPath "$stage/periph-in-one.exe" -Algorithm SHA256).Hash.ToLowerInvariant()
    "$checksum  periph-in-one.exe" | Set-Content -Encoding ascii "$stage/SHA256SUMS.txt"
    $archive = Join-Path (Get-Location) "dist/periph-in-one-$($project.version)-windows-x64.zip"
    if (Test-Path -LiteralPath $archive) { throw "Archive already exists: $archive" }
    Compress-Archive -Path "$stage/*" -DestinationPath $archive
    Write-Host "Package: $archive"
    Write-Host "Staging: $stage"
} finally {
    Pop-Location
}
