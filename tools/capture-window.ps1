param([string]$Out, [int]$CropW = 0, [int]$CropH = 0)
# Render ONLY the Cyclops window (PrintWindow) to a PNG, without bringing it to the foreground or
# reading anything else on screen. Usage: tools\capture-window.ps1 -Out shot.png [-CropW 520 -CropH 50]
Add-Type -AssemblyName System.Drawing
Add-Type @"
using System; using System.Runtime.InteropServices;
public class PW {
 [DllImport("user32.dll")] public static extern bool PrintWindow(IntPtr h, IntPtr dc, uint f);
 [DllImport("user32.dll")] public static extern bool GetWindowRect(IntPtr h, out RECT r);
 [DllImport("user32.dll")] public static extern bool SetProcessDPIAware();
 public struct RECT { public int L, T, R, B; } }
"@
[PW]::SetProcessDPIAware() | Out-Null
$h = (Get-Process cyclops | Select-Object -First 1).MainWindowHandle
$r = New-Object PW+RECT
[PW]::GetWindowRect($h, [ref]$r) | Out-Null
$bmp = New-Object System.Drawing.Bitmap ($r.R - $r.L), ($r.B - $r.T)
$g = [System.Drawing.Graphics]::FromImage($bmp)
$dc = $g.GetHdc()
[PW]::PrintWindow($h, $dc, 2) | Out-Null   # PW_RENDERFULLCONTENT
$g.ReleaseHdc($dc)
if ($CropW -gt 0) { $bmp = $bmp.Clone((New-Object System.Drawing.Rectangle 0, 0, $CropW, $CropH), $bmp.PixelFormat) }
$bmp.Save($Out)
"rendered $($bmp.Width)x$($bmp.Height)"
