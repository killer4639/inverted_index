[CmdletBinding()]
param(
    [Parameter(Mandatory)]
    [string]$InputPath,

    [Parameter(Mandatory)]
    [string]$OutputPath
)

$resolvedInputPath = (Resolve-Path -LiteralPath $InputPath -ErrorAction Stop).Path
$resolvedOutputPath = [System.IO.Path]::GetFullPath($OutputPath)

if ($resolvedInputPath.Equals(
        $resolvedOutputPath,
        [System.StringComparison]::OrdinalIgnoreCase
    )) {
    throw "InputPath and OutputPath must be different."
}

if (Test-Path -LiteralPath $resolvedOutputPath) {
    throw "Output file already exists: $resolvedOutputPath"
}

$outputDirectory = [System.IO.Path]::GetDirectoryName($resolvedOutputPath)
if (-not [string]::IsNullOrEmpty($outputDirectory)) {
    [System.IO.Directory]::CreateDirectory($outputDirectory) | Out-Null
}

$temporaryPath = "$resolvedOutputPath.tmp-$([guid]::NewGuid())"
$utf8WithoutBom = [System.Text.UTF8Encoding]::new($false)
$reader = $null
$writer = $null
$linesRead = 0L
$linesWritten = 0L

try {
    $reader = [System.IO.StreamReader]::new(
        $resolvedInputPath,
        [System.Text.Encoding]::UTF8,
        $true,
        1MB
    )
    $writer = [System.IO.StreamWriter]::new(
        $temporaryPath,
        $false,
        $utf8WithoutBom,
        1MB
    )

    while (($line = $reader.ReadLine()) -ne $null) {
        $linesRead++

        # Replacing invalid runs with a space prevents adjacent words from merging.
        $cleanedLine = [regex]::Replace($line, '[^A-Za-z0-9]+', ' ').Trim()
        if ($cleanedLine.Length -eq 0) {
            continue
        }

        $writer.WriteLine($cleanedLine)
        $linesWritten++
    }

    $writer.Flush()
    $writer.Dispose()
    $writer = $null
    $reader.Dispose()
    $reader = $null

    Move-Item -LiteralPath $temporaryPath -Destination $resolvedOutputPath
}
finally {
    if ($null -ne $writer) {
        $writer.Dispose()
    }
    if ($null -ne $reader) {
        $reader.Dispose()
    }
    if (Test-Path -LiteralPath $temporaryPath) {
        Remove-Item -LiteralPath $temporaryPath
    }
}

Write-Output "Cleaned $linesRead line(s); wrote $linesWritten non-empty document(s)."
Write-Output "Output: $resolvedOutputPath"
