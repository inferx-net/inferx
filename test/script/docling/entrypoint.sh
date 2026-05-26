#!/bin/bash
# Route to the appropriate script based on command arguments
# Usage: docker run docling-test base_url=... api_key=...  (convert)
#        docker run docling-test crawl2md.py https://...  (crawl)
#        docker run docling-test count_tokens.py files/... (count tokens)

if [ $# -eq 0 ]; then
    echo "Usage: docling-test base_url=... api_key=... model=..." > /dev/stderr
    echo "   or: docling-test crawl2md.py https://..." > /dev/stderr
    echo "   or: docling-test count_tokens.py file1.md [file2.md]" > /dev/stderr
    exit 1
fi

# Route based on first argument
case "$1" in
    crawl2md.py)
        shift
        exec python -u /app/crawl2md.py "$@"
        ;;
    count_tokens.py)
        shift
        exec python -u /app/count_tokens.py "$@"
        ;;
    *)
        exec python -u /app/convert.py "$@"
        ;;
esac
