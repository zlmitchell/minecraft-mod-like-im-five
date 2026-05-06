# One-shot: build the Windows exe in a container and drop it in dist/.
#
#   pwsh ./scripts/build-exe.ps1
#
# First run takes ~10 min (downloads MSVC SDK + compiles Tauri); subsequent
# runs are 1-2 min from cargo cache mounts.

$ErrorActionPreference = "Stop"
$root = Resolve-Path (Join-Path $PSScriptRoot "..")
Push-Location $root
try {
    docker build -f Dockerfile.build-exe -o dist .
    if ($LASTEXITCODE -ne 0) { throw "docker build failed (exit $LASTEXITCODE)" }
    $exe = Join-Path $root "dist\minecraft-mod-like-im-five.exe"
    if (Test-Path $exe) {
        $size = [math]::Round((Get-Item $exe).Length / 1MB, 2)
        Write-Host ""
        Write-Host "Built: $exe ($size MB)" -ForegroundColor Green
    } else {
        throw "exe missing from dist/ after build"
    }
}
finally {
    Pop-Location
}
