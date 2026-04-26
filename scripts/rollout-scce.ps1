#!/usr/bin/env pwsh
# Copyright (c) 2024-2026 Ryan P. Walsh. All rights reserved.

param(
    [string]$ScceRoot = "C:\Users\react\scce",
    [string]$BaseUrl = "http://127.0.0.1:3000",
    [string]$ManifestPath = "",
    [string]$ReportPath = "",
    [switch]$LaunchServer,
    [switch]$SkipGuestConfig,
    [switch]$SkipCuratedCorpus,
    [switch]$RefreshSpectral,
    [switch]$AllowCaseMisses
)

Set-StrictMode -Version Latest
$ErrorActionPreference = "Stop"

$GraphosRoot = Split-Path $PSScriptRoot -Parent
$CastleHaleRoot = "C:\Users\react\castlehale_one"

if ([string]::IsNullOrWhiteSpace($ManifestPath)) {
    $ManifestPath = Join-Path $GraphosRoot "assets\evals\scce-rollout.json"
}
if ([string]::IsNullOrWhiteSpace($ReportPath)) {
    $ReportPath = Join-Path $GraphosRoot "artifacts\scce-rollout\latest.md"
}

function Write-Step {
    param([string]$Message)
    Write-Host "[rollout] $Message" -ForegroundColor Cyan
}

function Start-DetachedPowerShell {
    param(
        [Parameter(Mandatory = $true)][string]$WorkingDirectory,
        [Parameter(Mandatory = $true)][string]$Command
    )

    $escapedDir = $WorkingDirectory.Replace("'", "''")
    Start-Process -FilePath "powershell.exe" `
        -ArgumentList "-NoExit", "-Command", "Set-Location '$escapedDir'; $Command" `
        -WindowStyle Normal | Out-Null
}

function Wait-ScceReady {
    param([int]$TimeoutSeconds = 90)

    $deadline = (Get-Date).AddSeconds($TimeoutSeconds)
    $statusUrl = ($BaseUrl.TrimEnd('/')) + "/api/system/status"
    do {
        try {
            $status = Invoke-RestMethod -Method Get -Uri $statusUrl
            if ($status.healthy) {
                return $status
            }
        } catch {
        }
        Start-Sleep -Milliseconds 1500
    } while ((Get-Date) -lt $deadline)

    throw "SCCE server did not become healthy at $statusUrl within $TimeoutSeconds seconds."
}

function Invoke-ScceJson {
    param(
        [Parameter(Mandatory = $true)][ValidateSet("GET", "POST")] [string]$Method,
        [Parameter(Mandatory = $true)][string]$Path,
        [object]$Body
    )

    $uri = ($BaseUrl.TrimEnd('/')) + $Path
    if ($Method -eq "GET") {
        return Invoke-RestMethod -Method Get -Uri $uri
    }

    $json = if ($null -eq $Body) { "{}" } else { $Body | ConvertTo-Json -Depth 8 }
    return Invoke-RestMethod -Method Post -Uri $uri -ContentType "application/json" -Body $json
}

function Expand-ManifestPath {
    param([string]$PathValue)

    $expanded = $PathValue.Replace('$GraphosRoot', $GraphosRoot)
    $expanded = $expanded.Replace('$CastleHaleRoot', $CastleHaleRoot)
    $expanded = $expanded.Replace('$ScceRoot', $ScceRoot)
    return $expanded
}

function Find-WikipediaDump {
    $wikiRoot = Join-Path $ScceRoot "data\wiki"
    if (-not (Test-Path -LiteralPath $wikiRoot)) {
        return $null
    }

    $dump = Get-ChildItem -LiteralPath $wikiRoot -Filter "enwiki-*.xml*" -File -ErrorAction SilentlyContinue |
        Sort-Object LastWriteTime -Descending |
        Select-Object -First 1
    if ($dump) {
        return $dump.FullName
    }
    return $null
}

function New-RolloutCorpus {
    param([Parameter(Mandatory = $true)]$Manifest)

    $stamp = Get-Date -Format "yyyyMMdd-HHmmss"
    $corpusDir = Join-Path $GraphosRoot ("artifacts\scce-rollout\corpus-" + $stamp)
    New-Item -ItemType Directory -Path $corpusDir -Force | Out-Null

    $manifestLines = New-Object System.Collections.Generic.List[string]
    foreach ($entry in @($Manifest.corpusFiles)) {
        $source = Expand-ManifestPath ([string]$entry.path)
        if (-not (Test-Path -LiteralPath $source)) {
            Write-Warning "Skipping missing rollout corpus source: $source"
            continue
        }

        $alias = [string]$entry.alias
        if ([string]::IsNullOrWhiteSpace($alias)) {
            $alias = Split-Path -Leaf $source
        }

        $destination = Join-Path $corpusDir $alias
        Copy-Item -LiteralPath $source -Destination $destination -Force
        $manifestLines.Add("$alias <= $source")
    }

    if ($manifestLines.Count -eq 0) {
        throw "Rollout corpus assembly produced no files."
    }

    $manifestPath = Join-Path $corpusDir "corpus-manifest.txt"
    Set-Content -Path $manifestPath -Value $manifestLines -Encoding ascii
    return $corpusDir
}

function Escape-MarkdownCell {
    param([string]$Value)

    if ($null -eq $Value) {
        return ""
    }

    return $Value.Replace("|", "\|").Replace("`r", " ").Replace("`n", " ")
}

function Test-Answer {
    param(
        [Parameter(Mandatory = $true)][string]$Answer,
        [Parameter(Mandatory = $true)]$Case
    )

    $lower = $Answer.ToLowerInvariant()
    $missingKeywords = New-Object System.Collections.Generic.List[string]
    foreach ($keyword in @($Case.keywords)) {
        $needle = [string]$keyword
        if (-not $lower.Contains($needle.ToLowerInvariant())) {
            $missingKeywords.Add($needle)
        }
    }

    $failurePatterns = @(
        "couldn't find relevant evidence",
        "not trained enough yet",
        "did not contain a readable",
        "reported an error"
    )
    $failurePhrase = $null
    foreach ($pattern in $failurePatterns) {
        if ($lower.Contains($pattern)) {
            $failurePhrase = $pattern
            break
        }
    }

    $hasCitation = $Answer -match '\[doc:\d+'
    $requireCitation = [bool]$Case.requireCitation
    $passed = ($null -eq $failurePhrase) -and (($requireCitation -eq $false) -or $hasCitation) -and ($missingKeywords.Count -eq 0)

    return [pscustomobject]@{
        Passed = $passed
        HasCitation = $hasCitation
        MissingKeywords = @($missingKeywords)
        FailurePhrase = $failurePhrase
    }
}

if (-not (Test-Path -LiteralPath $ManifestPath)) {
    throw "Rollout manifest not found: $ManifestPath"
}

$manifest = Get-Content -LiteralPath $ManifestPath -Raw | ConvertFrom-Json

if (-not $SkipGuestConfig) {
    $setupScript = Join-Path $GraphosRoot "scripts\setup-ai-stack.ps1"
    $setupArgs = @(
        "-Backend", "scce",
        "-ScceRoot", $ScceRoot,
        "-ScceHost", "10.0.2.2",
        "-SccePort", "3000"
    )
    if ($LaunchServer) {
        $setupArgs += "-LaunchScce"
    }

    Write-Step "Configuring GraphOS modeld for SCCE-first routing."
    & $setupScript @setupArgs
} elseif ($LaunchServer) {
    Write-Step "Launching SCCE server without touching guest config."
    Start-DetachedPowerShell -WorkingDirectory $ScceRoot -Command "npm run dev:server"
}

Write-Step "Waiting for SCCE server readiness at $BaseUrl."
$statusBefore = Wait-ScceReady

$wikiDump = Find-WikipediaDump
if ($wikiDump) {
    Write-Step "Detected Wikipedia dump in SCCE data path: $wikiDump"
}

$corpusDir = $null
if (-not $SkipCuratedCorpus) {
    Write-Step "Assembling curated rollout corpus from GraphOS and CastleHale sources."
    $corpusDir = New-RolloutCorpus -Manifest $manifest
    Write-Step "Ingesting curated rollout corpus: $corpusDir"
    $null = Invoke-ScceJson -Method POST -Path "/api/ingest" -Body @{
        rootPath = $corpusDir
        scope = "VAULT"
    }
}

if ($RefreshSpectral) {
    Write-Step "Refreshing SCCE spectral model for the rollout corpus."
    $null = Invoke-ScceJson -Method POST -Path "/api/training/refresh-spectral" -Body @{ k = 16 }
}

$statusAfter = Invoke-ScceJson -Method GET -Path "/api/system/status"

$results = New-Object System.Collections.Generic.List[object]
foreach ($case in @($manifest.cases)) {
    $query = [string]$case.query
    Write-Step ("Running case '{0}'" -f [string]$case.id)
    $timer = [System.Diagnostics.Stopwatch]::StartNew()
    $response = Invoke-ScceJson -Method POST -Path "/api/chat" -Body @{
        conversationId = $null
        message = $query
        attachments = @()
    }
    $timer.Stop()

    $answer = [string]$response.message
    $verdict = Test-Answer -Answer $answer -Case $case
    $results.Add([pscustomobject]@{
        Id = [string]$case.id
        Query = $query
        Answer = $answer
        DurationMs = [math]::Round($timer.Elapsed.TotalMilliseconds, 1)
        Passed = [bool]$verdict.Passed
        HasCitation = [bool]$verdict.HasCitation
        MissingKeywords = @($verdict.MissingKeywords)
        FailurePhrase = [string]$verdict.FailurePhrase
    })
}

$totalCases = $results.Count
$passedCases = (@($results | Where-Object { $_.Passed })).Count
$averageLatency = if ($totalCases -gt 0) {
    [math]::Round((($results | Measure-Object -Property DurationMs -Average).Average), 1)
} else {
    0
}
$citationCount = (@($results | Where-Object { $_.HasCitation })).Count
$wikiNote = if ($wikiDump) { "detected at `"$wikiDump`"" } else { "not detected" }

$reportDir = Split-Path -Parent $ReportPath
New-Item -ItemType Directory -Path $reportDir -Force | Out-Null

$report = New-Object System.Collections.Generic.List[string]
$report.Add("# SCCE Rollout Report")
$report.Add("")
$report.Add(("Generated: {0}" -f (Get-Date).ToString("yyyy-MM-dd HH:mm:ss zzz")))
$report.Add(("Base URL: `{0}`" -f $BaseUrl))
$report.Add(("Configured guest backend: `scce`"))
$report.Add(("Wikipedia dump: {0}" -f $wikiNote))
$report.Add(("Docs before: {0} | docs after: {1}" -f [int]$statusBefore.docs, [int]$statusAfter.docs))
if ($corpusDir) {
    $report.Add(("Curated corpus: `{0}`" -f $corpusDir))
}
$report.Add("")
$report.Add("## Summary")
$report.Add("")
$report.Add(("- Cases passed: **{0}/{1}**" -f $passedCases, $totalCases))
$report.Add(("- Responses with citations: **{0}/{1}**" -f $citationCount, $totalCases))
$report.Add(("- Average latency: **{0} ms**" -f $averageLatency))
$report.Add("")
$report.Add("| Case | Pass | Citation | Latency ms | Missing keywords |")
$report.Add("| --- | --- | --- | ---: | --- |")
foreach ($row in $results) {
    $missing = if ($row.MissingKeywords.Count -gt 0) {
        [string]::Join(", ", $row.MissingKeywords)
    } else {
        ""
    }
    $report.Add((
        "| {0} | {1} | {2} | {3} | {4} |" -f
        (Escape-MarkdownCell $row.Id),
        ($(if ($row.Passed) { "PASS" } else { "FAIL" })),
        ($(if ($row.HasCitation) { "yes" } else { "no" })),
        $row.DurationMs,
        (Escape-MarkdownCell $missing)
    ))
}

foreach ($row in $results) {
    $report.Add("")
    $report.Add(("## {0}" -f $row.Id))
    $report.Add("")
    $report.Add(("Query: {0}" -f $row.Query))
    $report.Add("")
    $report.Add(("Verdict: {0}" -f ($(if ($row.Passed) { "PASS" } else { "FAIL" }))))
    if (-not [string]::IsNullOrWhiteSpace($row.FailurePhrase)) {
        $report.Add(("Failure phrase: `{0}`" -f $row.FailurePhrase))
    }
    if ($row.MissingKeywords.Count -gt 0) {
        $report.Add(("Missing keywords: `{0}`" -f ([string]::Join(", ", $row.MissingKeywords))))
    }
    $report.Add("")
    $report.Add("```text")
    $report.Add($row.Answer)
    $report.Add("```")
}

Set-Content -Path $ReportPath -Value $report -Encoding utf8

Write-Step ("Report written to {0}" -f $ReportPath)
Write-Host ("[rollout] Cases passed: {0}/{1}" -f $passedCases, $totalCases) -ForegroundColor Green
Write-Host ("[rollout] Average latency: {0} ms" -f $averageLatency) -ForegroundColor Green

if ((-not $AllowCaseMisses) -and ($passedCases -ne $totalCases)) {
    throw ("SCCE rollout scoreboard has failing cases ({0}/{1} passed)." -f $passedCases, $totalCases)
}
