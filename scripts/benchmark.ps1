param(
    [string]$Url = "http://127.0.0.1:8080/demo",
    [string]$Duration = "30s",
    [ValidateRange(1, 100000)]
    [int]$Concurrency = 64
)

$ErrorActionPreference = "Stop"
if (-not (Get-Command hey -ErrorAction SilentlyContinue)) {
    throw "hey is required: https://github.com/rakyll/hey"
}

Write-Host "Relay load run: url=$Url duration=$Duration concurrency=$Concurrency"
& hey -z $Duration -c $Concurrency $Url
if ($LASTEXITCODE -ne 0) {
    throw "hey exited with status $LASTEXITCODE"
}
