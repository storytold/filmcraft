# Install the FilmCraft fidelity bridge panel into Adobe Premiere Pro (Windows; untested so far).
# Run as the user who runs Premiere. Copies .\bridge to the per-user extension folder and turns on
# CEP's PlayerDebugMode (needed for an unsigned panel). Then restart Premiere, open a project, and
# open Window > Extensions > FilmCraft Fidelity once.
$ErrorActionPreference = "Stop"
$here = Split-Path -Parent $MyInvocation.MyCommand.Path
$dest = Join-Path $env:APPDATA "Adobe\CEP\extensions\org.filmcraft.fidelity.bridge"
if ($args -contains "--uninstall") { Remove-Item -Recurse -Force $dest -ErrorAction SilentlyContinue; Write-Host "removed $dest"; exit 0 }
foreach ($v in 9..15) {
  $key = "HKCU:\Software\Adobe\CSXS.$v"
  New-Item -Path $key -Force | Out-Null
  New-ItemProperty -Path $key -Name PlayerDebugMode -Value "1" -PropertyType String -Force | Out-Null
}
Remove-Item -Recurse -Force $dest -ErrorAction SilentlyContinue
New-Item -ItemType Directory -Path $dest -Force | Out-Null
Copy-Item -Recurse -Force (Join-Path $here "bridge\*") $dest
Write-Host "installed $dest"
Write-Host "next: restart Premiere Pro, open a project, then Window > Extensions > FilmCraft Fidelity (once)."
