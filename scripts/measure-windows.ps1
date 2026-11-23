[CmdletBinding()]
param(
    [string]$Executable = (Join-Path $PSScriptRoot '..\target\release\winycast.exe'),
    [ValidateRange(1, 86400)]
    [int]$IdleSeconds = 30,
    [ValidateRange(0, 1000)]
    [int]$PanelCycles = 0
)

$ErrorActionPreference = 'Stop'
$exe = (Resolve-Path -LiteralPath $Executable).Path
if (Get-Process -Name ([IO.Path]::GetFileNameWithoutExtension($exe)) -ErrorAction SilentlyContinue) {
    throw 'Exit the running Winycast instance before starting a separate measurement.'
}

if (-not ('Winycast.Measurement' -as [type])) {
    Add-Type @'
using System;
using System.Runtime.InteropServices;

namespace Winycast {
    public static class Measurement {
        [DllImport("user32.dll")]
        private static extern IntPtr GetForegroundWindow();
        [DllImport("user32.dll")]
        private static extern uint GetWindowThreadProcessId(IntPtr window, out uint processId);
        [DllImport("user32.dll", SetLastError = true)]
        public static extern bool PostMessage(IntPtr window, uint message, UIntPtr wParam, IntPtr lParam);
        [DllImport("user32.dll")]
        public static extern uint GetGuiResources(IntPtr process, uint flag);
        [DllImport("user32.dll")]
        private static extern short GetAsyncKeyState(int key);
        [DllImport("user32.dll", SetLastError = true)]
        private static extern uint SendInput(uint count, Input[] inputs, int size);

        [StructLayout(LayoutKind.Sequential)]
        private struct Input {
            public uint Type;
            public InputData Data;
        }

        [StructLayout(LayoutKind.Explicit)]
        private struct InputData {
            [FieldOffset(0)] public KeyboardInput Key;
            [FieldOffset(0)] public MouseInput Mouse;
        }

        [StructLayout(LayoutKind.Sequential)]
        private struct KeyboardInput {
            public ushort Key, Scan;
            public uint Flags, Time;
            public UIntPtr ExtraInfo;
        }

        [StructLayout(LayoutKind.Sequential)]
        private struct MouseInput {
            public int X, Y;
            public uint MouseData, Flags, Time;
            public UIntPtr ExtraInfo;
        }

        public static void PressHotkey() {
            // Leave pre-existing physical modifiers alone.
            foreach (int key in new[] {0x10, 0x11, 0x12, 0x20, 0x5b, 0x5c}) {
                if (GetAsyncKeyState(key) < 0) {
                    throw new InvalidOperationException("Release the modifier or Space key before measuring.");
                }
            }
            const ushort control = 0xa2, alt = 0xa4, space = 0x20;
            int size = Marshal.SizeOf(typeof(Input));
            Input[] keys = {
                Key(control, false), Key(alt, false), Key(space, false),
                Key(space, true), Key(alt, true), Key(control, true)
            };
            uint sent = SendInput((uint)keys.Length, keys, size);
            if (sent != keys.Length) {
                int error = Marshal.GetLastWin32Error();
                if (sent > 0) {
                    SendInput(3, new[] {Key(space, true), Key(alt, true), Key(control, true)}, size);
                }
                throw new System.ComponentModel.Win32Exception(error, "Unable to send the test hotkey.");
            }
        }

        private static Input Key(ushort key, bool release) {
            return new Input {
                Type = 1,
                Data = new InputData { Key = new KeyboardInput { Key = key, Flags = release ? 2u : 0u } }
            };
        }

        public static IntPtr GetOwnedForegroundWindow(uint processId) {
            IntPtr window = GetForegroundWindow();
            if (window == IntPtr.Zero) return window;
            uint owner;
            GetWindowThreadProcessId(window, out owner);
            return owner == processId ? window : IntPtr.Zero;
        }
    }
}
'@
}

function Read-Trace {
    $stream = [IO.File]::Open($trace, [IO.FileMode]::Open, [IO.FileAccess]::Read, [IO.FileShare]::ReadWrite)
    $reader = [IO.StreamReader]::new($stream)
    try { return $reader.ReadToEnd() }
    finally { $reader.Dispose() }
}

function Wait-ForTrace {
    param([string]$Pattern, [int]$Offset, [int]$TimeoutMs)
    $deadline = [Diagnostics.Stopwatch]::StartNew()
    do {
        $log = Read-Trace
        $match = [regex]::Match($log.Substring([Math]::Min($Offset, $log.Length)), $Pattern)
        if ($match.Success) {
            return $match.Value
        }
        if ($instance.HasExited) {
            throw "Measurement process exited with code $($instance.ExitCode). See $trace"
        }
        Start-Sleep -Milliseconds 3
    } while ($deadline.ElapsedMilliseconds -lt $TimeoutMs)
    throw "Timed out waiting for '$Pattern'. See $trace"
}

function Read-Resources {
    $instance.Refresh()
    return [ordered]@{
        workingSetMiB = [Math]::Round($instance.WorkingSet64 / 1MB, 2)
        privateMiB = [Math]::Round($instance.PrivateMemorySize64 / 1MB, 2)
        cpuMs = $instance.TotalProcessorTime.TotalMilliseconds
        handles = $instance.HandleCount
        gdiObjects = [Winycast.Measurement]::GetGuiResources($instance.Handle, 0)
        userObjects = [Winycast.Measurement]::GetGuiResources($instance.Handle, 1)
    }
}

function Get-Timing {
    param([double[]]$Samples)
    if (!$Samples.Count) { return $null }
    $sorted = @($Samples | Sort-Object)
    return [ordered]@{
        samples = $sorted.Count
        firstMs = [Math]::Round($Samples[0], 2)
        medianMs = [Math]::Round($sorted[[Math]::Ceiling($sorted.Count * 0.5) - 1], 2)
        p95Ms = [Math]::Round($sorted[[Math]::Ceiling($sorted.Count * 0.95) - 1], 2)
        maxMs = [Math]::Round($sorted[-1], 2)
    }
}

function Send-WindowMessage {
    param([IntPtr]$Window, [uint32]$Message, [uint64]$Parameter = 0, [int]$Detail = 0)
    if (![Winycast.Measurement]::PostMessage($Window, $Message, [UIntPtr]::new($Parameter), [IntPtr]::new($Detail))) {
        throw [ComponentModel.Win32Exception]::new([Runtime.InteropServices.Marshal]::GetLastWin32Error())
    }
}

function Measure-Panel {
    $offset = (Read-Trace).Length
    $watch = [Diagnostics.Stopwatch]::StartNew()
    [Winycast.Measurement]::PressHotkey()
    $response = '(?m)^(?:visible: true, active: true, activation: [0-9]+ ms|show: [^\r\n]+)'
    $activation = Wait-ForTrace $response $offset 1200
    if ($activation.StartsWith('show:')) { throw $activation }
    $sample = [ordered]@{
        activated = $watch.Elapsed.TotalMilliseconds
        controllerActivation = [int]([regex]::Match($activation, '[0-9]+').Value)
        results = $null
        input = $null
        inputError = $null
    }
    $plainResults = '(?m)^search: Launcher, [1-9][0-9]* results'
    $null = Wait-ForTrace $plainResults $offset 1200
    $sample.results = $watch.Elapsed.TotalMilliseconds

    # GPUI installs its native input handler when it paints the focused search field.
    Start-Sleep -Milliseconds 75
    $panel = [Winycast.Measurement]::GetOwnedForegroundWindow($instance.Id)
    if ($panel -eq [IntPtr]::Zero) {
        $sample.inputError = 'The measurement panel lost foreground focus before the input sample.'
        $null = Wait-ForTrace '(?m)^visible: false,' $offset 1200
        return [pscustomobject]$sample
    }

    try {
        # WM_CHAR goes to the test HWND only. A private-use character should match no apps.
        $inputOffset = (Read-Trace).Length
        $watch.Restart()
        Send-WindowMessage $panel 0x0102 0xE800 1
        $null = Wait-ForTrace '(?m)^search: Launcher, 0 results' $inputOffset 1200
        $sample.input = $watch.Elapsed.TotalMilliseconds
    } catch {
        $sample.inputError = $_.Exception.Message
    } finally {
        # Clear the test character, then close through the system close path.
        Send-WindowMessage $panel 0x0100 0x08 0x000E0001
        Send-WindowMessage $panel 0x0010
        $null = Wait-ForTrace '(?m)^visible: false,' $offset 1200
    }
    return [pscustomobject]$sample
}

$directory = Join-Path (Join-Path $PSScriptRoot '..\target') ('measurement-' + [guid]::NewGuid().ToString('n'))
$directory = (New-Item -ItemType Directory -Path $directory).FullName
$trace = Join-Path $directory 'trace.log'
$instance = Start-Process -FilePath $exe -ArgumentList @('--trace-window', '--data-dir', ('"{0}"' -f $directory)) `
    -WindowStyle Hidden -RedirectStandardError $trace -PassThru
try {
    $indexed = Wait-ForTrace '(?m)^indexed: [0-9]+ apps' 0 15000
    $log = Read-Trace
    if ($log -notmatch '(?m)^resident: true' -or $log -match '(?m)^desktop: HotkeyUnavailable') {
        throw "The test instance could not start hidden with its default hotkey. See $trace"
    }
    Start-Sleep -Seconds 2
    $beforeIdle = Read-Resources
    $idleClock = [Diagnostics.Stopwatch]::StartNew()
    Start-Sleep -Seconds $IdleSeconds
    $afterIdle = Read-Resources
    $elapsedMs = $idleClock.Elapsed.TotalMilliseconds
    $metrics = [ordered]@{
        trace = $trace
        apps = [int]([regex]::Match($indexed, '[0-9]+').Value)
        idleSeconds = [Math]::Round($elapsedMs / 1000, 2)
        idleCpuPercentOfOneCore = [Math]::Round(100 * ($afterIdle.cpuMs - $beforeIdle.cpuMs) / $elapsedMs, 3)
        idleStart = $beforeIdle
        idleEnd = $afterIdle
    }
    if ($PanelCycles -gt 0) {
        $samples = [Collections.Generic.List[object]]::new()
        try {
            for ($cycle = 0; $cycle -lt $PanelCycles; $cycle++) {
                $samples.Add((Measure-Panel))
                if ($cycle -eq 0) { $metrics.afterFirstCycle = Read-Resources }
            }
        } catch {
            $metrics.panelError = $_.Exception.Message
        }
        $metrics.requestedPanelCycles = $PanelCycles
        $metrics.completedPanelCycles = $samples.Count
        $metrics.activation = Get-Timing ($samples | ForEach-Object activated)
        $metrics.controllerActivation = Get-Timing ($samples | ForEach-Object controllerActivation)
        $metrics.initialResults = Get-Timing ($samples | ForEach-Object results)
        $metrics.inputToResults = Get-Timing ($samples | Where-Object { $null -ne $_.input } | ForEach-Object input)
        $metrics.inputErrors = @($samples | ForEach-Object inputError | Where-Object { $_ })
        Start-Sleep -Seconds 3
        $metrics.afterPanelCycles = Read-Resources
    }
    $metrics | ConvertTo-Json -Depth 4
} finally {
    try {
        if (!$instance.HasExited) {
            $instance.Kill()
            if (!$instance.WaitForExit(5000)) {
                throw 'The measurement process did not exit in time.'
            }
        }
    } finally {
        $instance.Dispose()
    }
}
