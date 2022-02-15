#!/bin/bash
#
# Given an image, sets the EXIF data that http-server will expect. (or, checks that all the expected
# tags are there, with --check)
#
# This script is temporary; we'll eventually make an improved handler script that does the
# bulk-uploading of images & their data as well.
#
# Currently, there's also a couple things missing; that'll be added later.

RESET="\e[0m"
BOLD="\e[1m"
RED="\e[31m"
YELLOW="\e[33m"

COPYRIGHT='Max Sharnoff'
EXIFTOOL_INDENT='   > exiftool: '

usage="Usage $(basename "$0") [ --check ] <file>"

main () {
    parse_args $@

    if [[ $check == 'yes' ]]; then
        do_check "$file"
    else
        do_set "$file"
    fi
}

parse_args () {
    check='no'

    if [[ $# -eq 1 ]]; then
        file="$1"
    elif [[ $# -eq 2 ]]; then
        if [[ "$1" != '--check' ]]; then
            echo "$usage"
            exit 1
        fi
        check='yes'
        file="$2"
    else
        echo "$usage"
        exit 1
    fi
}

# The full list of attributes that the image *must* have are:
#
#     Attribute    | Tag(s)
#     -------------|-------
#     Title        | ImageDescription
#     Time         | DateTimeOriginal, OffsetTimeOriginal
#     Description  | UserComment
#     Copyright    | Copyright
#
# Technically, copyright isn't required by http-server, but it's good to have there anyways.
# There's also the tags relating to GPS position and Lens info, which aren't required, but must have
# all or none:
#
#     GPS position | GPSLongitude, GPSLongitudeRef, GPSLatitude, GPSLatitudeRef,
#     Lens ID      | LensMake, LensModel
#
# Checking for these is then pretty straightforward

gps_tags=('GPSLongitude' 'GPSLongitudeRef' 'GPSLatitude' 'GPSLatitudeRef')
lens_tags=('LensMake' 'LensModel')

# Usage: do_check <file>
do_check () {
    set -e

    required=('ImageDescription' 'DateTimeOriginal' 'OffsetTimeOriginal' 'UserComment' 'Copyright')
    failed='no'
    
    out=$( exiftool -s "${required[@]/#/-}" "${gps_tags[@]/#/-}" "${lens_tags[@]/#/-}" "$1" | grep -o '^[[:alpha:]]*' | sort)
    required_lines="$(echo ${required[@]} | tr ' ' '\n' | sort)"
    gps_lines="$(echo ${gps_tags[@]} | tr ' ' '\n' | sort)"
    lens_lines="$(echo ${lens_tags[@]} | tr ' ' '\n' | sort)"

    missing_required="$( comm <(echo "$required_lines") <(echo "$out") -2 -3 | tr '\n' ' ' )"
    missing_gps="$( comm <(echo "$gps_lines") <(echo "$out") -2 -3 | tr '\n' ' ' )"
    missing_lens="$( comm <(echo "$lens_lines") <(echo "$out") -2 -3 | tr '\n' ' ' )"

    if [[ ! -z "$missing_required" ]]; then
        echo "$1: error: missing required tag(s): $missing_required"
        failed='yes'
    fi
    
    gps_missing_count=$( echo "$missing_gps" | wc -w )
    if [[ "$gps_missing_count" -eq ${#gps_tags[@]} ]]; then
        echo "$1: warning: no GPS data"
    elif [[ "$gps_missing_count" -ne 0 ]]; then
        echo "$1: error: partial GPS data: missing tag(s) $missing_gps"
        failed='yes'
    fi

    lens_missing_count=$( echo "$missing_lens" | wc -w )
    if [[ "$lens_missing_count" -eq ${#lens_tags[@]} ]]; then
        echo "$1: warning: no Lens data"
    elif [[ "$lens_missing_count" -ne 0 ]]; then
        echo "$1: error: partial Lens data: missing tag(s) $missing_lens"
        failed='yes'
    fi

    if [[ $failed == 'yes' ]]; then
        exit 1
    fi
}

# Usage: do_set <file>
do_set () {
    set -e

    set_title "$1"
    set_description "$1"
    set_time "$1"
    set_copyright "$1"
    set_gps "$1"
    set_lens "$1"
}

set_title () {
    existing_title=$(exiftool -s3 -ImageDescription "$1")
    if [[ ! -z "$existing_title" ]]; then
        echo ":: Current image title: '$existing_title'"
        if [[ "$(prompt 'Do you wish to change it?')" == 'no' ]]; then
            return
        fi
    fi

    while true; do
        echo -n ":: Please enter a title: "
        read title

        if [[ -z "$title" ]]; then
            echo '   > Title must be non-empty.'
            continue
        fi

        echo ":: Setting title"
        exiftool -overwrite_original "-ImageDescription=$title" "$1" | indent "$EXIFTOOL_INDENT"
        break
    done
}

set_description () {
    existing_description=$(exiftool -json -UserComment "$1" | jq -r '.[0].UserComment // empty')

    if [[ ! -z "$existing_description" ]]; then
        echo ':: A description already exists for this image'
        if [[ "$(prompt 'Do you wish to change it?')" == 'no' ]]; then
            return
        fi
    else
        existing_description="description for file '$1'..."
    fi

    if [[ -z "$EDITOR" ]]; then
        echo 'No $EDITOR set; cannot ask for description'
        exit 1
    fi

    tmpfile=$(mktemp /tmp/set-exif.XXXXXX)
    echo "$existing_description" > $tmpfile
    $EDITOR $tmpfile
    desc="$(cat $tmpfile)"
    rm $tmpfile

    echo ':: Setting description...'
    exiftool -overwrite_original "-UserComment=$desc" "$1" | indent "$EXIFTOOL_INDENT"
}

set_time () {
    existing_time=$(exiftool -s3 -DateTimeOriginal "$1")
    existing_offset=$(exiftool -s3 -OffsetTimeOriginal "$1")

    if [[ ( ! -z "$existing_time" ) && ( ! -z "$existing_offset" ) ]]; then
        echo ":: A time & offset are already set for this image: $existing_time $existing_offset"
        if [[ "$(prompt 'Do you wish to change either?')" == 'no' ]]; then
            return
        fi
    fi

    if [[ ! -z "$existing_time" ]]; then
        echo ":: Existing local time is: $existing_time"
        while true; do
            echo -n "Set local time? [(d)elta/(o)verwrite/(n)o] "
            read resp
            case "$resp" in
                'd' | 'delta')
                    delta_fmt=" \`(+|-)\$Y:\$M:\$D \$HH?:\$MM:\$SS\`"
                    delta_regexp='(+|-)[[:digit:]]+:[[:digit:]]+:[[:digit:]]+ [[:digit:]]{1,2}:[[:digit:]]{2}:[[:digit:]]{2}'
                    echo ":: A valid 'delta' string matches$delta_fmt, with all of the letters replaced with numbers indicating the year/month/day/hour/minute/second difference"
                    while true; do
                        echo -n ":: Delta? "
                        read delta
                        if echo "$delta" | grep -qE "$delta_regexp"; then
                            break
                        fi

                        echo " > Delta must exactly match the format$delta_fmt."
                    done

                    sign=${delta::1}
                    change=${delta:1}
                    exiftool -overwrite_original "-DateTimeOriginal$sign=$change" "$1"
                    ;;
                'o' | 'overwrite')
                    write_localtime "$1"
                    ;;
                'n' | 'no')
                    break
                    ;;
                *)
                    continue
                    ;;
            esac
            break
        done
    else
        write_localtime "$1"
    fi

    if [[ ! -z "$existing_offset" ]]; then
        if [[ $(prompt ":: Set time offset? (currently $existing_offset)") == 'no' ]]; then
            return
        fi
    fi

    while true; do
        echo -n 'Enter offset (±H:MM): '
        read offset

        if [[ ! -z "$offset" ]]; then
            break
        fi
    done

    exiftool -overwrite_original "-OffsetTimeOriginal=$offset" "$1" | indent "$EXIFTOOL_INDENT"
}

write_localtime () {
    while true; do
        echo -n ':: Enter DateTime (`YYYY:MM:DD HH:MM:SS`): '
        read datetime
        if [[ ! -z "$datetime" ]]; then
            break
        fi
    done

    exiftool -overwrite_original "-DateTimeOriginal=$datetime" "$1" | indent "$EXIFTOOL_INDENT"
}

set_copyright () {
    existing_copyright=$(exiftool -s3 -Copyright "$1")
    if [[ -z "$existing_copyright" ]]; then
        echo ":: Setting copyright to '$COPYRIGHT'..."
        exiftool -overwrite_original "-Copyright=$COPYRIGHT" "$1" | indent "$EXIFTOOL_INDENT"
    else
        echo ":: Copyright is already set to '$existing_copyright'"
    fi
}

set_gps () {
    # removes the leading sign from GPSLongitude/GPSLatitude
    rm_sign='(select(. != null) | (. | tostring)[1:])'
    ref_letter='(select(. != null) | .[:1])'

    jq_prog=".[] | del(.SourceFile) | .GPSLatitude |= $rm_sign | .GPSLongitude |= $rm_sign | .GPSLatitudeRef |= $ref_letter | .GPSLongitudeRef |= $ref_letter"

    gps_fields=$(exiftool -json -coordFormat '%+.6f' "${gps_tags[@]/#/-}" "$1" | jq "$jq_prog")

    lat=$(echo "$gps_fields" | jq -r '.GPSLatitude // empty')
    lon=$(echo "$gps_fields" | jq -r '.GPSLongitude // empty')
    lat_ref=$(echo "$gps_fields" | jq -r '.GPSLatitudeRef // empty')
    lon_ref=$(echo "$gps_fields" | jq -r '.GPSLongitudeRef // empty')

    if [[ "$(echo "$gps_fields" | jq 'length')" -eq 4 ]]; then
        if [[ "$lat_ref" == 'S' ]]; then
            lat_display="-$lat"
        else
            lat_display="$lat"
        fi

        if [[ "$lon_ref" == 'W' ]]; then
            lon_display="-$lon"
        else
            lon_display="$lon"
        fi

        echo ":: Current GPS coords: $lat_display, $lon_display"
        if [[ "$(prompt "Change GPS?")" == 'no' ]]; then
            return
        fi
    else
        echo ':: No existing GPS coords'
        if [[ "$(prompt "Set GPS?")" == 'no' ]]; then
            return
        fi
    fi

    while true; do
        echo -n ':: Please enter new GPS coords (format: `[-]LAT, [-]LON`): '
        read new_coords

        coord_re='-?[[:digit:]]+(\.[[:digit:]]+)?'
        re="^$coord_re,[[:space:]]*$coord_re\$"
        if ! echo "$new_coords" | grep -qE "$re"; then
            echo " > Coordinates must match the format \`$re\`"
            continue
        fi

        break
    done

    # See: https://stackoverflow.com/a/5257398
    coords_arr=(${new_coords//,/ })

    lat=${coords_arr[0]}
    lon=${coords_arr[1]}

    if [[ $lat == -* ]]; then
        lat_ref='S'
        lat=${lat:1} # strip leading '-'
    else
        lat_ref='N'
    fi
    if [[ $lon == -* ]]; then
        lon_ref='W'
        lon=${lon:1} # strip leading '-'
    else
        lon_ref='E'
    fi

    exiftool -overwrite_original \
        "-GPSLongitude=$lon" "-GPSLongitudeRef=$lon_ref" \
        "-GPSLatitude=$lat" "-GPSLatitudeRef=$lat_ref" \
        "$1" \
        | indent "$EXIFTOOL_INDENT"

    # exiftool -s3 -coordFormat %+.6f -GPSPosition <file>
}

# Exiftool will lie about the LensMake and LensModel fields if it's able to get the information from
# elsewhere. So we need to explicitly overwrite them with the calculated information.
#
# And sometimes it's able to get LensModel but not LensMake.
set_lens () {
    lens_fields="$(exiftool -json -s -LensMake -LensModel "$1" | jq '.[0] | del(.SourceFile)')"

    lens_make="$(echo "$lens_fields" | jq -r '.LensMake // empty')"
    lens_model="$(echo "$lens_fields" | jq -r '.LensModel // empty')"

    if [[ ! -z "$lens_model" ]]; then
        if [[ -z "$lens_make" ]]; then
            echo ":: LensModel found ($lens_model) but no LensMake found"
            while true; do
                echo -n ': Please enter the LensMake (manufacturer): '
                read make

                if [[ -z "$make" ]]; then
                    echo '    > LensMake must be non-empty.'
                    continue
                fi

                make="=$make"
                break
            done
        else
            make='<LensMake'
        fi

        echo ':: Setting lens make & model...'
        exiftool -overwrite_original "-LensMake$make" '-LensModel<LensModel' "$1" \
            | indent "$EXIFTOOL_INDENT"
    else
        echo ':: Warning: no lens model'
    fi
}

# Usage: <cmd> | indent <string>
#
# Prefixes <string> to all lines on STDIN and outputs them. The prefix string must not contain any
# slashes; this will cause failures
indent () {
    sed -e "s/^/$1/g"
}

# Usage: prompt <question>
#
# Advanced usage: if [[ "$(prompt <question>)" == 'yes'/'no' ]]; then ...
#
# Asks for the user to answer 'yes' ('y') or 'no' ('n') to the question. Repeats the question until
# the answer is one of those two.
prompt () {
    while true; do
        echo -n "$1 [y/n] " > /dev/tty
        read resp </dev/tty

        case "$resp" in
            'y' | 'yes' | 'Y' | 'Yes')
                echo 'yes'
                return
                ;;
            'n' | 'no' | 'N' | 'No')
                echo 'no'
                return
                ;;
            *);;
        esac
    done
}

main $@
