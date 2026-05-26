# Sample call with ~/test as input directory
# Config passed as key=value arguments
# API key from environment variable

# ============================================================
# Docling PDF converter
# ============================================================
# API key from environment variable (set API_KEY in host shell first)

sudo docker run --rm \
  -v /home/brad/test/input:/input \
  -v /home/brad/test/output:/output \
  -e "API_KEY=$API_KEY" \
  docling-test \
    base_url=https://model.inferx.net/funccall/tn-a3t79iogb2/endpoints/Qwen3.6-35B-A3B-FP8/v1 \
    api_key=$API_KEY \
    model=Qwen/Qwen3.6-35B-A3B-FP8

# ============================================================
# Web crawler (examples)
# ============================================================

# Basic: crawl entire vLLM docs (up to 200 pages, depth 5)
sudo docker run --rm \
  -v /home/brad/test/output:/output \
  docling-test \
    crawl2md.py https://docs.vllm.ai/en/latest/usage/

# Limit to 50 pages, max depth 2
sudo docker run --rm \
  -v /home/brad/test/output:/output \
  docling-test \
    crawl2md.py https://docs.vllm.ai/en/latest/ --max-pages 50 --max-depth 2

# Only crawl API reference section
sudo docker run --rm \
  -v /home/brad/test/output:/output \
  docling-test \
    crawl2md.py https://docs.vllm.ai/en/latest/ --include-paths /api-docs,/cli

# Increase timeout
sudo docker run --rm \
  -v /home/brad/test/output:/output \
  docling-test \
    crawl2md.py https://docs.vllm.ai/en/latest/ --timeout 60

# ============================================================
# Token counter
# ============================================================

# Count tokens in the llm.md generated from crawling
sudo docker run --rm \
  -v /home/brad/test/output:/output \
  docling-test \
    count_tokens.py /output/llm.md

# Count tokens in merged output with per-file breakdown
sudo docker run --rm \
  -v /home/brad/test/output:/output \
  docling-test \
    count_tokens.py /output/page_0001_Using_vLLM.md /output/llm.md --verbose

# Pipe from stdin
cat /home/brad/test/output/llm.md | sudo docker run --rm -i \
  --entrypoint python docling-test -c "exec(open('/app/count_tokens.py').read().replace('sys.argv[1:]', 'sys.argv[1:] + [\"-\"]]') + '\nmain()')" 2>/dev/null
sudo docker run --rm \
  -v /home/brad/test/output:/output \
  docling-test \
    count_tokens.py /output/page_*.md --verbose