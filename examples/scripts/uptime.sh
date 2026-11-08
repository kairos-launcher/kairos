#!/bin/sh
# Output that arrives line by line streams into the panel; Ctrl+C stops the script.
# @kairos.title System status
# @kairos.keyword status
# @kairos.mode output
# @kairos.format markdown
# @kairos.icon utilities-system-monitor
echo "**Uptime:** $(uptime -p 2>/dev/null || uptime)"
echo
echo "**Disk:**"
df -h / | tail -n 1 | awk '{ print "- " $3 " of " $2 " used (" $5 ")" }'
echo
echo "**Memory:**"
free -h 2>/dev/null | awk '/^Mem/ { print "- " $3 " of " $2 " used" }'
