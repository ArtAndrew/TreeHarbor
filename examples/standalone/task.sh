#!/bin/busybox sh
set -eu

same_bytes() {
    printf '%s' "$1" | /bin/busybox cmp -s - same.txt
}

mode=$1
shift
case "$mode" in
    isolation|overlap-edit)
        test "${TH_TEST_SECRET+x}" != x
        test ! -e /var/run/docker.sock
        test ! -r "$2"
        if test "$mode" = overlap-edit; then
            base='base
'
            same_bytes "$base"
        fi
        printf '%s' "$1" > same.txt
        if test "$mode" = overlap-edit; then
            while test ! -f overlap-release; do
                /bin/busybox sleep 0.05
            done
        fi
        ;;
    check|check-one-of)
        printf '%s\n' 'one check' >> check-executions
        for expected in "$@"; do
            if same_bytes "$expected"; then exit 0; fi
        done
        exit 23
        ;;
    *) exit 64 ;;
esac
