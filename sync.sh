#!/bin/sh
#
# Private script for synchronizing the local files back to sharnoff.io
#
# Optional args:
#  --bin <name>  Copies the binary linked to by bin/<name> to the server's bin/<name>
#
# The --bin argument is typically used with target/release/http-server 

set -e

dir=$(dirname "$0")

print_usage_and_exit () {
    echo 'Usage: sync.sh [ --bin <name> ]'
    exit 1
}

# Usage: <cmd> | indent <string>
#
# Prefixes <string> to all lines on STDIN and outputs them. The prefix string must not contain any
# slashes; this will cause failures
indent () {
    sed -e "s/^/$1/g"
}

INDENT_STR="    " # 4 spaces

if [[ $# -eq 2 ]]; then
    if [[ "$1" != '--bin' ]]; then
        print_usage_and_exit
    fi

    if [[ ! -f "$dir/bin/$2" ]]; then
        echo "No binary $2 in bin"
        exit 1
    fi

    cmd="scp $(realpath "$dir/bin/$2") max@sharnoff.io:~/website/bin/$2"

    echo ":: Synchronizing $2 to sharnoff.io:~/website/bin..."
    echo ":: > $cmd"
    $cmd | indent '    '
    echo ':: Done'
    exit 0
elif [[ $# -ne 0 ]]; then
    print_usage_and_exit
fi

dry_run_cmd="rsync -avcn --delete --exclude-from=.rsync-exclude $dir max@sharnoff.io:~/website"
actual_cmd="rsync -avcz --delete --exclude-from=.rsync-exclude $dir max@sharnoff.io:~/website"

echo ':: Performing dry-run...'
echo ":: > $dry_run_cmd"

$dry_run_cmd | sed -e 's/^deleting/\x1b[31mdeleting\x1b[0m/g' | indent "$INDENT_STR"

while true; do
    echo -n ':: Confirm? [y/n] '

    read resp
    case "$resp" in
        'y')
            break
            ;;
        'n')
            echo ':: Exiting without synchronizing.'
            exit 0
            ;;
        *)
            continue
            ;;
    esac
done

echo ':: Performing full synchronization...'
echo ":: > $actual_cmd"

cmd_output="$($actual_cmd | indent "$INDENT_STR" | tee /dev/tty)"

echo ':: Done'

updates=""
if grep -q "^${INDENT_STR}content/photos/." <(echo "$cmd_output"); then
    updates="photos"
fi

if grep -qE "^${INDENT_STR}(deleting |)content/blog-posts/." <(echo "$cmd_output"); then
    updates="$updates blog"
fi

if [[ ! -z "$updates" ]]; then

    echo ":: Send update signal '$updates'..."
    echo ":: > ssh max@sharnoff.io \"echo '$updates' >> website/updated\""

    ssh max@sharnoff.io "echo '$updates' >> website/updated" | indent "$INDENT_STR"

    echo ':: Done'
fi

if grep -q "^${INDENT_STR}caddy.json" <(echo "$cmd_output"); then
    echo -n ':: caddy.json has been updated. Reload? [y/n] '
    while true; do
        read resp
        case "$resp" in
            'y')
                echo ':: Reloading caddy...'
                echo ":: > ssh max@sharnoff.io 'caddy reload --config=website/caddy.json'"

                ssh max@sharnoff.io 'caddy reload --config=website/caddy.json' | indent "$INDENT_STR"

                echo ':: Done'
                break
                ;;
            'n')
                break
                ;;
            *)
                echo -n ':: Copy over caddy.json? [y/n] '
                ;;
        esac
    done
fi
