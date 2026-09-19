param(
    [string]$MountPoint
)

$ErrorActionPreference = "Stop"

function Get-UsbSerialPorts {
    if ($IsWindows) {
        return [System.IO.Ports.SerialPort]::GetPortNames()
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

function Find-BootselMount {
    if ($IsWindows) {
        $volume = Get-Volume -FileSystemLabel "RP2350" -ErrorAction SilentlyContinue |
            Select-Object -First 1
        if ($volume -and $volume.DriveLetter) {
            return "$($volume.DriveLetter):\"
        }
        return $null
    }

    if ($IsMacOS) {
        $candidate = "/Volumes/RP2350"
        if (Test-Path $candidate) {
            return $candidate
        }
        return $null
    }

    $linuxCandidates = @(
        "/media/$env:USER/RP2350",
        "/run/media/$env:USER/RP2350",
        "/mnt/RP2350"
    )
    return $linuxCandidates | Where-Object { Test-Path $_ } | Select-Object -First 1
}

function Start-SerialMonitor {
    param(
        [Parameter(Mandatory)]
        [string]$PortName
    )

    $serialPort = [System.IO.Ports.SerialPort]::new($PortName, 115200)
    $serialPort.ReadTimeout = 250

    Write-Host "Starting serial monitor on $PortName. Stop with Ctrl-C."
    $serialPort.Open()
    try {
        while ($true) {
            $text = $serialPort.ReadExisting()
            if ($text.Length -gt 0) {
                [Console]::Write($text)
            }
            Start-Sleep -Milliseconds 25
        }
    }
    finally {
        if ($serialPort.IsOpen) {
            $serialPort.Close()
        }
        $serialPort.Dispose()
    }
}

function Import-DotEnv {
    param(
        [Parameter(Mandatory)]
        [string]$Path
    )

    foreach ($rawLine in Get-Content -LiteralPath $Path) {
        $line = $rawLine.Trim()
        if (-not $line -or $line.StartsWith("#")) {
            continue
        }

        $parts = $line.Split("=", 2)
        if ($parts.Count -ne 2) {
            throw "Invalid .env entry: expected NAME=value"
        }

        $name = $parts[0].Trim()
        $value = $parts[1].Trim()
        if ($name -notmatch "^[A-Za-z_][A-Za-z0-9_]*$") {
            throw "Invalid .env variable name: $name"
        }

        if ($value.Length -ge 2) {
            $first = $value[0]
            $last = $value[$value.Length - 1]
            if (($first -eq '"' -and $last -eq '"') -or ($first -eq "'" -and $last -eq "'")) {
                $value = $value.Substring(1, $value.Length - 2)
            }
        }

        [Environment]::SetEnvironmentVariable($name, $value, "Process")
    }
}

$repoRoot = Split-Path -Parent $PSScriptRoot
$elfPath = Join-Path $repoRoot "target/thumbv8m.main-none-eabihf/release/pico-data-logger"
$uf2Path = Join-Path $repoRoot "target/pico-data-logger.uf2"
$envFile = Join-Path $repoRoot ".env"

Set-Location $repoRoot

if (Test-Path -LiteralPath $envFile) {
    Import-DotEnv -Path $envFile
    Write-Host "Loaded build configuration from .env"
}

if (-not (Get-Command cargo -ErrorAction SilentlyContinue)) {
    throw "cargo is not installed or is not on PATH"
}

if (-not (Get-Command elf2uf2-rs -ErrorAction SilentlyContinue)) {
    throw "elf2uf2-rs is not installed; see the README prerequisites"
}

$converterHelp = elf2uf2-rs convert --help 2>&1 | Out-String
if ($converterHelp -notmatch "rp2350-arm-s") {
    throw "This elf2uf2-rs does not support the RP2350 UF2 family; reinstall the revision documented in README.md"
}

Write-Host "Building release firmware..."
cargo build --release
if ($LASTEXITCODE -ne 0) {
    throw "Release build failed with exit code $LASTEXITCODE"
}

Write-Host "Converting ELF to an RP2350 Arm Secure UF2..."
elf2uf2-rs convert --family rp2350-arm-s $elfPath $uf2Path
if ($LASTEXITCODE -ne 0) {
    throw "UF2 conversion failed with exit code $LASTEXITCODE"
}

if (-not $MountPoint) {
    $MountPoint = Find-BootselMount
}

if (-not $MountPoint -or -not (Test-Path $MountPoint)) {
    throw "The RP2350 BOOTSEL volume was not found. Hold BOOTSEL while connecting the Pico, then rerun this script or pass -MountPoint."
}

$portsBefore = @(Get-UsbSerialPorts)

Write-Host "Copying firmware to $MountPoint..."
Copy-Item -LiteralPath $uf2Path -Destination $MountPoint -Force

Write-Host "Waiting for the BOOTSEL volume to disappear..."
$bootselDisappeared = $false
foreach ($attempt in 1..15) {
    if (-not (Test-Path $MountPoint)) {
        $bootselDisappeared = $true
        break
    }
    Start-Sleep -Seconds 1
}

if (-not $bootselDisappeared) {
    throw "$MountPoint is still mounted; the Pico did not accept or reboot from the UF2"
}

Write-Host "Firmware copied and the Pico rebooted. Waiting for USB serial..."
foreach ($attempt in 1..10) {
    $portsAfter = @(Get-UsbSerialPorts)
    $newPorts = @($portsAfter | Where-Object { $_ -notin $portsBefore })
    if ($newPorts.Count -gt 0) {
        Write-Host "New serial device: $($newPorts -join ', ')"
        Start-SerialMonitor -PortName $newPorts[0]
        exit 0
    }
    if ($portsAfter.Count -gt 0) {
        Write-Host "Available serial device: $($portsAfter -join ', ')"
        Write-Host "The Pico may have reused its previous device name."
        Start-SerialMonitor -PortName $portsAfter[0]
        exit 0
    }
    Start-Sleep -Seconds 1
}

Write-Warning "Firmware was flashed, but no USB serial device appeared. Check the operating system's USB and serial-device inventory."
exit 2
