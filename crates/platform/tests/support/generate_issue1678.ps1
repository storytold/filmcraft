param([Parameter(Mandatory=$true)][string]$OutputDirectory)
$ErrorActionPreference = 'Stop'
$ffmpeg = (Get-Command ffmpeg -ErrorAction Stop).Source
$ffprobe = (Get-Command ffprobe -ErrorAction Stop).Source
New-Item -ItemType Directory -Path $OutputDirectory -Force | Out-Null
$fixtureRoot = (Resolve-Path -LiteralPath $OutputDirectory).Path
$cases = @(
    @{ Name='sdr'; Pixel='yuv420p'; Primaries='bt709'; Transfer='bt709'; Matrix='bt709'; Donor='6:6:6' },
    @{ Name='pq'; Pixel='yuv420p10le'; Primaries='bt2020'; Transfer='smpte2084'; Matrix='bt2020nc'; Donor='1:1:1' },
    @{ Name='hlg'; Pixel='yuv420p10le'; Primaries='bt2020'; Transfer='arib-std-b67'; Matrix='bt2020nc'; Donor='1:1:1' }
)
$records = @()
foreach ($case in $cases) {
    $actual = Join-Path $fixtureRoot ($case.Name + '-actual.mp4')
    $donor = Join-Path $fixtureRoot ($case.Name + '-donor.mp4')
    # Purely procedural footage. HDR cases test declared signal handling; the testsrc signal
    # is not a physically calibrated HDR scene. No HDR mastering/content-light metadata added.
    $frameSignal = 'setparams=color_primaries=' + $case.Primaries + ':color_trc=' + $case.Transfer + ':colorspace=' + $case.Matrix + ':range=limited'
    $generateArgs = @('-n','-v','error','-f','lavfi','-i','testsrc2=size=640x360:rate=24','-vf',$frameSignal,
        '-frames:v','48','-an','-c:v','libaom-av1','-cpu-used','8','-threads','4','-row-mt','1',
        '-crf','34','-b:v','0','-g','24','-lag-in-frames','0','-pix_fmt',$case.Pixel,
        '-color_primaries',$case.Primaries,'-color_trc',$case.Transfer,'-colorspace',$case.Matrix,
        '-color_range','tv','-movflags','+write_colr',$actual)
    & $ffmpeg @generateArgs
    if ($LASTEXITCODE -ne 0) { throw "Fixture generation failed: $($case.Name)" }
    $values = $case.Donor.Split(':')
    $metadata = 'av1_metadata=color_primaries=' + $values[0] + ':transfer_characteristics=' + $values[1] + ':matrix_coefficients=' + $values[2]
    & $ffmpeg -n -v error -i $actual -map 0:v:0 -an -c:v copy -bsf:v $metadata -movflags +write_colr $donor
    if ($LASTEXITCODE -ne 0) { throw "Configuration donor generation failed: $($case.Name)" }
    foreach ($path in @($actual,$donor)) {
        $file = Get-Item -LiteralPath $path
        if ($file.Length -gt 8MB) { throw "Fixture exceeds 8 MiB cap: $path" }
        $probe = & $ffprobe -v error -select_streams v:0 -show_entries stream=codec_name,profile,width,height,pix_fmt,color_range,color_space,color_transfer,color_primaries,nb_frames -of json $path
        if ($LASTEXITCODE -ne 0) { throw "ffprobe failed: $path" }
        $probeText = $probe -join [Environment]::NewLine
        $probeValue = $probeText | ConvertFrom-Json
        if ($path -eq $actual) {
            $signal = $probeValue.streams[0]
            if ($signal.color_primaries -ne $case.Primaries -or $signal.color_transfer -ne $case.Transfer -or $signal.color_space -ne $case.Matrix) {
                throw "Actual fixture metadata does not match requested signal: $path"
            }
        }
        Set-Content -LiteralPath ($path + '.ffprobe.json') -Value $probeText -Encoding utf8
        $records += @{ file=$file.Name; bytes=$file.Length; sha256=(Get-FileHash -LiteralPath $path -Algorithm SHA256).Hash; probe=$probeValue }
        if (($records | Measure-Object -Property bytes -Sum).Sum -gt 25MB) { throw 'Combined fixture size exceeds 25 MiB budget.' }
    }
}
@{ generated_utc=[DateTime]::UtcNow.ToString('o'); ffmpeg=$ffmpeg; ffmpeg_version=(& $ffmpeg -version | Select-Object -First 1); ffprobe=$ffprobe; records=$records; acceptance='not run'; provenance='procedural lavfi testsrc2; external fixture generator only' } |
    ConvertTo-Json -Depth 12 | Set-Content -LiteralPath (Join-Path $fixtureRoot 'manifest.json') -Encoding utf8
Write-Output "Generated six bounded oracle inputs in $fixtureRoot. Hardware acceptance has not run."
