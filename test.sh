#!/bin/bash

if [ ! -f fs.img ]; then
    dd if=/dev/zero of=fs.img bs=1M count=16 status=none
fi

OUTPUT=$(cargo run 2>&1)

if echo "$OUTPUT" | grep -q "STORAGE-OK"; then
    echo "Test PASSED: Found STORAGE-OK in output"
    exit 0
else
    echo "Test FAILED: STORAGE-OK not found in output"
    echo "Actual output:"
    echo "$OUTPUT"
    exit 1
fi
