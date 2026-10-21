#!/bin/sh
target() { printf '%s\n' ok; }
caller() { target; }
caller
