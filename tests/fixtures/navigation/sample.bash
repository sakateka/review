#!/usr/bin/env -S bash -eu
function target() { printf '%s\n' ok; }
caller() { target; }
caller
