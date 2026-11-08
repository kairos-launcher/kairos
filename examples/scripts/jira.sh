#!/bin/sh
# Takes what you type after the keyword: `jira PROJ-123` opens that issue.
# Set JIRA_URL in your environment, or edit the default below.
# @kairos.title Open Jira issue
# @kairos.keyword jira
# @kairos.argument Issue key
# @kairos.mode silent
# @kairos.icon web-browser
xdg-open "${JIRA_URL:-https://jira.example.com}/browse/$1"
