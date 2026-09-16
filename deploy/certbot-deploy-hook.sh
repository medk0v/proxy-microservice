#!/usr/bin/env bash
set -Eeuo pipefail

case "${RENEWED_LINEAGE:-}" in
    /etc/letsencrypt/live/prx.azimuthglobal.co) ;;
    *) exit 0 ;;
esac

/usr/sbin/nginx -t
/usr/bin/systemctl reload nginx.service
