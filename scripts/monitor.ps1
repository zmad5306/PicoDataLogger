param(
    [string]$SerialPort
)

$ErrorActionPreference = "Stop"

function Get-UsbSerialPorts {
    if ($IsWindows) {
        return [System.IO.Ports.SerialPort]::GetPortNames() | Sort-Object -Unique
    }

    $patterns = if ($IsMacOS) {
        @("/dev/cu.usbmodem*", "/dev/tty.usbmodem*")
    } else {
        @("/dev/ttyACM*", "/dev/ttyUSB*")
    }

    return @(
        foreach ($pattern in $patterns) {
            Get-ChildItem -Path $pattern -ErrorAction SilentlyContinue |
                ForEach-Object { $_.FullName }
        }
    ) | Sort-Object -Unique
}

function Get-PicoUsbSerialPorts {
    if (-not $IsWindows) {
        return @()
    }

    return @(
        Get-CimInstance Win32_SerialPort -ErrorAction SilentlyContinue |
            Where-Object { $_.PNPDeviceID -like "USB\VID_C0DE&PID_CAFE*" } |
            ForEach-Object { $_.DeviceID }
    ) | Sort-Object -Unique
}

function Start-SerialMonitor {
    param(
        [Parameter(Mandatory)]
        [string]$PortName
    )

    $port = [System.IO.Ports.SerialPort]::new($PortName, 115200)
    $port.ReadTimeout = 250
    $port.Encoding = [System.Text.Encoding]::UTF8

    $openError = $null
    foreach ($attempt in 1..10) {
        try {
            $port.Open()
            $openError = $null
            break
        }
        catch {
            $openError = $_.Exception.GetBaseException().Message
            if ($attempt -eq 1) {
                Write-Warning "$PortName is not ready or is in use; retrying for up to 10 seconds."
            }
            if ($attempt -lt 10) {
                Start-Sleep -Seconds 1
            }
        }
    }

    if (-not $port.IsOpen) {
        $port.Dispose()
        throw "Could not open $PortName after 10 attempts. Close any other serial monitor using the port and retry. Last error: $openError"
    }

    Write-Host "Starting serial monitor on $PortName. Stop with Ctrl-C."
    try {
        while ($true) {
            $text = $port.ReadExisting()
            if ($text.Length -gt 0) {
                [Console]::Write($text)
            }
            Start-Sleep -Milliseconds 25
        }
    }
    finally {
        if ($port.IsOpen) {
            $port.Close()
        }
        $port.Dispose()
    }
}

$availablePorts = @(Get-UsbSerialPorts)

if ($SerialPort) {
    if ($SerialPort -notin $availablePorts) {
        throw "Serial device $SerialPort was not found. Available devices: $($availablePorts -join ', ')"
    }
    Start-SerialMonitor -PortName $SerialPort
    exit 0
}

$candidatePorts = if ($IsWindows) {
    Get-PicoUsbSerialPorts
} else {
    $availablePorts
}
$candidatePorts = @($candidatePorts)

if ($candidatePorts.Count -eq 0) {
    throw "No Pico USB serial device was found. Available serial devices: $($availablePorts -join ', ')"
}
if ($candidatePorts.Count -gt 1) {
    throw "Multiple USB serial devices were found: $($candidatePorts -join ', '). Rerun with -SerialPort to select one."
}

Start-SerialMonitor -PortName $candidatePorts[0]
