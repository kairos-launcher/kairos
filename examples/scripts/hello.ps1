# Windows: copy into %APPDATA%\kairos\scripts\ and type `hello-ps`.
# @kairos.title Hello from PowerShell
# @kairos.keyword hello-ps
# @kairos.mode output
Write-Output "Hello, $env:USERNAME!"
Write-Output ("It is " + (Get-Date -Format "dddd, HH:mm") + ".")
