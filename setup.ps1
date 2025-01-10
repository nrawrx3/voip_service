# PowerShell script to manage the voip_service
param(
        [string]$ProjectDir = (Get-Location).Path
)

# Function to ensure the service binary is built
function Build-Service {
        Write-Host "Building the service..."
        cd $ProjectDir
        $env:RUSTFLAGS = "-C target-feature=+crt-static --cfg tokio_unstable"
        cargo build --features windows-service
        if (-Not (Test-Path "$ProjectDir\target\debug\voip_service.exe")) {
                Write-Error "Build failed. Ensure 'cargo' is installed and properly configured."
                exit 1
        }
        Write-Host "Build completed successfully."
}

# Function to create the Windows service
function Create-Service {
        Write-Host "Creating the service..."
        sc.exe create voipservice binPath= "`"$ProjectDir\target\release\voip_service.exe`""
        if ($LASTEXITCODE -eq 0) {
                Write-Host "Service created successfully."
        }
        else {
                Write-Error "Failed to create the service."
        }
}

# Function to start the Windows service
function Start-Service {
        Write-Host "Starting the service..."
        sc.exe start voipservice
        if ($LASTEXITCODE -eq 0) {
                Write-Host "Service started successfully."
        }
        else {
                Write-Error "Failed to start the service."
        }
}

# Function to view the service log
function View-Log {
        $logFile = "C:\voip_service.log"
        if (Test-Path $logFile) {
                Write-Host "Displaying log file:"
                Get-Content $logFile
        }
        else {
                Write-Error "Log file not found at $logFile."
        }
}

# Function to stop the Windows service
function Stop-Service {
        Write-Host "Stopping the service..."
        sc.exe stop voipservice
        if ($LASTEXITCODE -eq 0) {
                Write-Host "Service stopped successfully."
        }
        else {
                Write-Error "Failed to stop the service."
        }
}

# Function to delete the Windows service
function Delete-Service {
        Write-Host "Deleting the service..."
        sc.exe delete voipservice
        if ($LASTEXITCODE -eq 0) {
                Write-Host "Service deleted successfully."
        }
        else {
                Write-Error "Failed to delete the service."
        }
}

# Menu options
Write-Host "Choose an action:"
Write-Host "1. Build Service"
Write-Host "2. Create Service"
Write-Host "3. Start Service"
Write-Host "4. View Log"
Write-Host "5. Stop Service"
Write-Host "6. Delete Service"
Write-Host "7. Exit"

while ($true) {
        $choice = Read-Host "Enter your choice (1-7)"
        switch ($choice) {
                1 { Build-Service }
                2 { Create-Service }
                3 { Start-Service }
                4 { View-Log }
                5 { Stop-Service }
                6 { Delete-Service }
                7 { break }
                default { Write-Error "Invalid choice. Please enter a number between 1 and 7." }
        }
}
