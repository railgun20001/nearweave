param(
    [Parameter(Mandatory = $true)]
    [ValidateSet("Add", "Remove")]
    [string]$Action,

    [Parameter(Mandatory = $true)]
    [string]$Program
)

$ErrorActionPreference = "Stop"
$udpRuleName = "NearWeave LAN Discovery (UDP 37991)"
$tcpRuleName = "NearWeave LAN Transfer (Dynamic TCP)"
$legacyUdpRuleName = [Text.Encoding]::Unicode.GetString(
    [Convert]::FromBase64String("3YRlaCAAQFzfV1F/0VOwcyAAKABVAEQAUAAgADMANwA5ADkAMQApAA==")
)
$legacyTcpRuleName = [Text.Encoding]::Unicode.GetString(
    [Convert]::FromBase64String("3YRlaCAAQFzfV1F/IE+TjyAAKACoUgFgIABUAEMAUAApAA==")
)
$legacyEnglishUdpRuleName = [Text.Encoding]::Unicode.GetString(
    [Convert]::FromBase64String("QgBsAHUAZQBiAHIAaQBkAGcAZQAgAEwAQQBOACAARABpAHMAYwBvAHYAZQByAHkAIAAoAFUARABQACAAMwA3ADkAOQAxACkA")
)
$legacyEnglishTcpRuleName = [Text.Encoding]::Unicode.GetString(
    [Convert]::FromBase64String("QgBsAHUAZQBiAHIAaQBkAGcAZQAgAEwAQQBOACAAVAByAGEAbgBzAGYAZQByACAAKABEAHkAbgBhAG0AaQBjACAAVABDAFAAKQA=")
)

foreach ($ruleName in @($udpRuleName, $tcpRuleName)) {
    Get-NetFirewallRule -DisplayName $ruleName -ErrorAction SilentlyContinue |
        Remove-NetFirewallRule -ErrorAction SilentlyContinue
}

if ($Action -eq "Add") {
    foreach ($ruleName in @(
        $legacyUdpRuleName,
        $legacyTcpRuleName,
        $legacyEnglishUdpRuleName,
        $legacyEnglishTcpRuleName
    )) {
        Get-NetFirewallRule -DisplayName $ruleName -ErrorAction SilentlyContinue |
            Remove-NetFirewallRule -ErrorAction SilentlyContinue
    }

    New-NetFirewallRule `
        -DisplayName $udpRuleName `
        -Description "Allow NearWeave discovery from the local IPv4 subnet and Windows hotspot" `
        -Direction Inbound `
        -Action Allow `
        -Enabled True `
        -Profile Any `
        -RemoteAddress LocalSubnet4 `
        -Program $Program `
        -Protocol UDP `
        -LocalPort 37991 `
        -EdgeTraversalPolicy Block | Out-Null

    New-NetFirewallRule `
        -DisplayName $tcpRuleName `
        -Description "Allow NearWeave dynamic TCP transfer from the local IPv4 subnet and Windows hotspot" `
        -Direction Inbound `
        -Action Allow `
        -Enabled True `
        -Profile Any `
        -RemoteAddress LocalSubnet4 `
        -Program $Program `
        -Protocol TCP `
        -LocalPort Any `
        -EdgeTraversalPolicy Block | Out-Null
}
