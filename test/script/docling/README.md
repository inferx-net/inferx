# Docling PDF to Markdown Converter & Web Crawler

Tools for converting documents and documentation sites to optimized markdown.

- **convert.py** - Convert PDF, DOCX, PPTX, HTML files to markdown using Docling with remote LLM
- **crawl2md.py** - Crawl a documentation website (e.g. vLLM docs) and merge all pages into a single markdown file
- **count_tokens.py** - Count tokens in markdown files using the Qwen3.6-35B-A3B-FP8 tokenizer

## Web Crawler (crawl2md.py)

Crawl documentation websites and merge all pages into `merged.md` for RAG/KV cache usage.

### Usage

```bash
# Basic: crawl entire site (up to 200 pages, depth 5)
sudo docker run --rm \
  -v /home/brad/test/output:/output \
  docling-test \
    crawl2md.py https://docs.vllm.ai/en/latest/usage/

# Limit to 50 pages, max depth 2
sudo docker run --rm \
  -v /home/brad/test/output:/output \
  docling-test \
    crawl2md.py https://docs.vllm.ai/en/latest/ --max-pages 50 --max-depth 2

# Only crawl specific sections
sudo docker run --rm \
  -v /home/brad/test/output:/output \
  docling-test \
    crawl2md.py https://docs.vllm.ai/en/latest/ --include-paths /api-docs,/cli
```

### Options

- `--max-pages N`       Max pages to crawl (default: 200)
- `--max-depth N`       Max link depth from start URL (default: 5)
- `--exclude PATTERN`   Regex pattern to skip URLs
- `--include-paths X,Y` Only crawl these URL paths
- `--timeout N`         HTTP timeout in seconds (default: 30)

### Features

- Recursive crawling within the same domain
- Respects max depth and page limits
- Rate limiting (0.5s between requests)
- Individual pages saved as `page_XXXX_page-title.md`
- Merged into `merged.md` (full), `llm.md` (LLM-optimized), `merged_compressed.md` (lossless)

## Usage

### Basic Usage (Lossless Compression)

```bash
sudo docker run --rm \
  -v /home/brad/test/input:/input \
  -v /home/brad/test/output:/output \
   -e "API_KEY=YOUR_API_KEY_HERE" \
   docling-test \
   base_url=https://model.inferx.net/funccall/tn-a3t79iogb2/endpoints/Qwen3-Coder-Next-FP8/v1 \
   api_key=YOUR_API_KEY_HERE \
   model=Qwen/Qwen3-Coder-Next-FP8
```

### With DSPy Optimization (Experimental)

```bash
sudo docker run --rm \
  -v /home/brad/test/input:/input \
  -v /home/brad/test/output:/output \
   -e "API_KEY=YOUR_API_KEY_HERE" \
   -e "USE_DSPY=true" \
   docling-test \
   base_url=https://model.inferx.net/funccall/tn-a3t79iogb2/endpoints/Qwen3-Coder-Next-FP8/v1 \
   api_key=YOUR_API_KEY_HERE \
   model=Qwen/Qwen3-Coder-Next-FP8
```

## Output Files

- `/home/brad/test/output/merged.md`: Original Docling output with summary and document boundaries
- `/home/brad/test/output/optimized.md`: DSPy or lossless compressed version (KV cache optimized)
- `/home/brad/test/output/llm.md`: LLM-ready format with system instructions for LLM prompts

### LLM-Optimized Output Format (`llm.md`)

```
## SYSTEM INSTRUCTION
You are analyzing technical documents. Follow these rules:

1. **Grounding**: Only use information from the documents below
2. **Missing content**: If formulas appear as placeholders, infer meaning from surrounding text
3. **Images**: Treat diagram descriptions as contextual hints
4. **Code**: Explain code blocks when relevant
5. **Citations**: Always cite both filename AND section number

   **Correct examples:**
   - `[bitcoin.pdf, Section 4 - Proof-of-Work]`
   - `[bitcoin.pdf, Section 11]`
   - `[bitcoin.pdf, Section 5, Step 3]`

   **Incorrect examples:**
   - `[bitcoin.pdf]` (missing section)
   - `Section 4` (missing filename)

## DOCUMENTS

## Document: [filename]
[document content]

## QUESTION
[User's question here]
```

**Features:**
- Ready-to-use system instruction for LLM prompts
- Citation format guidance with examples
- Diagrams replaced with contextual descriptions
- Section headers preserved for easy reference

## Configuration

### Arguments (passed to container)

- `base_url` - LLM endpoint URL (default: inferx endpoint)
- `api_key` - API key for authentication
- `model` - Model name without provider prefix (default: `Qwen/Qwen3-Coder-Next-FP8`)

### Environment Variables

- `API_KEY` - Alternative way to provide API key
- `USE_DSPY=true` - Enable DSPy optimization (disabled by default)

## Supported File Types

- PDF (.pdf)
- Word (.docx)
- PowerPoint (.pptx)
- HTML (.html, .htm)
- Markdown (.md)
- Text (.txt)

All files in `/input` and subdirectories are processed recursively.

## Processing Time

- **Docling**: ~10 seconds per file (uses remote LLM + OCR)
- **Lossless compression**: ~0.00 seconds (fast)
- **DSPy optimization**: ~40-150 seconds (chunked, depends on file size)
- **LLM optimization**: Built-in (no extra time - part of Docling output)

## Notes

- DSPy is **experimental** and may occasionally corrupt output (validation fallback to original)
- Lossless compression is **safe and deterministic** but provides minimal token savings (~1-2%)
- DSPy can achieve ~10-20% reduction with the same content
- OCR models are pre-downloaded in Docker image

## Token Counter (count_tokens.py)

Count tokens in one or more markdown files using the Qwen3.6-35B-A3B-FP8 tokenizer (same as the inferx LLM).

### Usage

```bash
# Single file
sudo docker run --rm \
  -v /home/brad/test/output:/output \
  docling-test \
    count_tokens.py /output/llm.md

# Multiple files (merged before counting)
sudo docker run --rm \
  -v /home/brad/test/output:/output \
  docling-test \
    count_tokens.py /output/page_*.md --verbose

# After crawling
sudo docker run --rm \
  -v /home/brad/test/output:/output \
  docling-test \
    count_tokens.py /output/merged.md
```

### Output

Shows per-file token counts, total tokens, estimated cost, and context window usage percentage.
