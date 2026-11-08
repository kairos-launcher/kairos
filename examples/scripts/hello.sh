#!/bin/sh
# A script command: copy this file into ~/.config/kairos/scripts/ and type `hello`.
# @kairos.title Hello
# @kairos.mode output
# @kairos.description Says hello in the output panel
echo "Hello, $(id -un)!"
echo "It is $(date '+%A, %H:%M')."
