#!/usr/bin/env python3
"""
Crawl a documentation website and merge all pages into a single markdown file.

Usage:
    python crawl2md.py <url> [options]

Environment variables:
    OUTPUT_DIR - Output directory (default: /output)

Examples:
    python crawl2md.py https://docs.vllm.ai/en/latest/usage/
    python crawl2md.py https://docs.vllm.ai/en/latest/ --max-pages 50
    python crawl2md.py https://docs.vllm.ai/en/latest/ --depth 3
"""

import sys
import os
import re
import time
import hashlib
from urllib.parse import urljoin, urlparse, urlunparse
from pathlib import Path
from datetime import datetime

import httpx
from bs4 import BeautifulSoup
import trafilatura
import markdownify


def get_config():
    """Parse command-line arguments."""
    args = {}
    raw = sys.argv[1:]
    if not raw:
        print("Usage: crawl2md.py <url> [--max-pages N] [--max-depth N] [--exclude PATTERN] [--include-paths PATH1,PATH2]", file=sys.stderr)
        sys.exit(1)

    args["url"] = raw[0]

    i = 1
    while i < len(raw):
        if raw[i] == "--max-pages" and i + 1 < len(raw):
            args["max_pages"] = int(raw[i + 1]); i += 2
        elif raw[i] == "--max-depth" and i + 1 < len(raw):
            args["max_depth"] = int(raw[i + 1]); i += 2
        elif raw[i] == "--exclude" and i + 1 < len(raw):
            args["exclude"] = raw[i + 1]; i += 2
        elif raw[i] == "--include-paths" and i + 1 < len(raw):
            args["include_paths"] = [p.strip() for p in raw[i + 1].split(",")]; i += 2
        elif raw[i] == "--timeout" and i + 1 < len(raw):
            args["timeout"] = int(raw[i + 1]); i += 2
        else:
            i += 1

    # Defaults
    args.setdefault("max_pages", 200)
    args.setdefault("max_depth", 5)
    args.setdefault("exclude", None)
    args.setdefault("include_paths", None)
    args.setdefault("timeout", 30)
    args.setdefault("output_dir", os.environ.get("OUTPUT_DIR", "/output"))

    return args


def get_existing_files(output_dir: Path, prefix: str):
    """Get list of previously generated files for resume support."""
    if not output_dir.exists():
        return []
    existing = sorted(output_dir.glob(f"{prefix}_*.md"))
    return existing


def save_doc(index: int, filename: str, content: str, output_dir: Path, prefix: str = "page"):
    """Save a single document to a numbered file."""
    safe_name = re.sub(r'[^\w._-]', '_', filename)
    filepath = output_dir / f"{prefix}_{index:04d}_{safe_name}.md"
    with open(filepath, "w", encoding="utf-8") as f:
        f.write(content)
    return filepath


def extract_page_content(html: str, url: str) -> tuple[str, str]:
    """Extract markdown content from an HTML page using trafilatura with fallback."""
    # Primary: trafilatura (best for documentation sites)
    extracted = trafilatura.extract(
        html,
        include_comments=False,
        include_tables=True,
        include_formatting=True,
        include_links=True,
        url=url,
    )

    if extracted and len(extracted) > 100:
        # Clean up excessive whitespace
        extracted = re.sub(r'\n{4,}', '\n\n\n', extracted)
        extracted = re.sub(r' +$', '', extracted, flags=re.MULTILINE)
        return extracted.strip(), "trafilatura"

    # Fallback: markdownify + custom selectors for docs sites
    selectors = ["article", ".content", ".main-content", "#content", ".document",
                 "main", ".entry-content", "div[class*='doc']"]
    for sel in selectors:
        try:
            soup = BeautifulSoup(html, "html.parser")
            content_el = soup.select_one(sel)
            if content_el and len(content_el.get_text(strip=True)) > 200:
                md = markdownify.markdownify(str(content_el), heading_style="ATX", list_style="-")
                if len(md) > 100:
                    md = re.sub(r'\n{4,}', '\n\n\n', md)
                    return md.strip(), "markdownify"
        except Exception:
            continue

    # Last resort: whole page
    md = markdownify.markdownify(html, heading_style="ATX")
    return md if len(md) > 50 else "", "markdownify-entire"


def parse_internal_links(html: str, base_url: str) -> set[str]:
    """Extract all valid internal links from a page."""
    soup = BeautifulSoup(html, "html.parser")
    links = set()
    base_netloc = urlparse(base_url).netloc

    for a in soup.find_all("a", href=True):
        href = a["href"].split("#")[0].split("?")[0]  # strip fragments/query
        if not href:
            continue
        # Skip non-content links
        if href.startswith("#") or href.startswith("javascript:") or href.startswith("mailto:"):
            continue
        # Skip external links
        if href.startswith("http://") or href.startswith("https://"):
            parsed_href = urlparse(href)
            if parsed_href.netloc and parsed_href.netloc != base_netloc:
                continue
            if parsed_href.netloc:
                parsed = parsed_href
            else:
                parsed = urlparse(urljoin(base_url, href))
        else:
            # Resolve relative links against the page URL directly
            # MkDocs Material uses ../ links; filter results outside /latest/ below
            parsed = urlparse(urljoin(base_url, href))

        # Must match base domain
        if parsed.netloc != base_netloc:
            continue
        # Filter out links outside the /latest/ section (e.g., navigation links that crawl outside docs)
        if "/latest/" not in parsed.path and parsed.path != "/latest/":
            continue

        # Must end in .html or be a directory with no extension
        path = parsed.path
        if "." in os.path.basename(path):
            suffix = os.path.splitext(path)[1].lower()
            if suffix not in (".html", ".htm", ".md") and not path.endswith("/"):
                continue
        # Remove trailing slash for canonicalization
        path = re.sub(r'/$', '', path)
        # MkDocs uses directory-style URLs; for non-html paths, append trailing slash
        if not path.endswith((".html", ".htm", ".md", "")):
            path = path.rstrip("/") + "/"

        links.add(urlunparse(parsed._replace(path=path, query="", fragment="")))

    return links


def should_crawl(url: str, config: dict) -> bool:
    """Check if URL should be crawled based on filters."""
    if config.get("exclude"):
        if re.search(config["exclude"], url):
            return False
    if config.get("include_paths"):
        parsed = urlparse(url)
        if not any(parsed.path.startswith(p) for p in config["include_paths"]):
            return False
    # Reject sub-project paths (e.g., /projects/tpu/, /models/..., etc.)
    parsed = urlparse(url)
    path_part = parsed.path.lstrip("/")
    if path_part.startswith("projects/") or any(path_part.startswith(p + "/") for p in ["models", "docs"]):
        return False
    # Only allow the main docs path pattern (e.g., /en/latest/...)
    if not path_part.startswith("en/latest/") and path_part != "en/latest":
        return False
    # Only html/md pages or root paths
    suffix = os.path.splitext(parsed.path)[1].lower()
    if suffix and suffix not in (".html", ".htm", ".md"):
        if not parsed.path.endswith("/"):
            return False
    # Skip deep API stub pages (docstring-generated links, usually > 7 segments)
    parts = path_part.split("/")
    if len(parts) >= 3 and parts[2] == "api" and len(parts) > 8:
        return False
    return True


def crawl_site(start_url: str, config: dict):
    """Crawl site and return list of (url, title, content, extraction_method)."""
    base_url = start_url.rstrip("/")
    base_netloc = urlparse(base_url).netloc

    seen_urls: set[str] = set()
    results: list[dict] = []
    pages_left = [{"url": base_url, "depth": 0}]

    all_internal_cache: dict[str, set[str]] = {}
    html_cache: dict[str, str] = {}

    # Rate limiting
    last_request = 0
    min_delay = 0.5

    client = httpx.Client(
        follow_redirects=True,
        timeout=config["timeout"],
        headers={
            "User-Agent": "Mozilla/5.0 (X11; Linux x86_64) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/125.0.0.0 Safari/537.36",
            "Accept": "text/html,application/xhtml+xml,application/xml;q=0.9,image/avif,image/webp,*/*;q=0.8",
            "Accept-Language": "en-US,en;q=0.5",
            "Accept-Encoding": "identity",
        },
    )

    print(f"Fetched: {base_url}")

    while pages_left and len(results) < config["max_pages"]:
        page = pages_left.pop(0)
        url = page["url"]
        depth = page["depth"]

        if depth > config["max_depth"]:
            continue
        if url in seen_urls:
            continue
        if not should_crawl(url, config):
            seen_urls.add(url)
            continue

        # Rate limiting
        elapsed = time.time() - last_request
        if elapsed < min_delay:
            time.sleep(min_delay - elapsed)
        last_request = time.time()

        seen_urls.add(url)

        try:
            resp = client.get(url, follow_redirects=True)
            if resp.status_code != 200:
                print(f"  Skipping ({resp.status_code}): {url}")
                continue

            html = resp.text
            html_cache[url] = html

            content, method = extract_page_content(html, url)
            if not content or len(content) < 500:
                # Pages with very little content are likely directory listings/stubs
                print(f"  Skipping (too short: {len(content) if content else 0} chars): {url}")
                continue

            # Extract title
            soup = BeautifulSoup(html, "html.parser")
            title = soup.title.string.strip() if soup.title and soup.title.string else url.split("/")[-1]

            # Strip site name suffix (e.g., "Using vLLM - vLLM" -> "Using vLLM")
            # Remove everything after ' - ' or ' - ' followed by site name
            title = re.sub(r'\s*-\s+.*$', '', title).strip()

            results.append({
                "url": url,
                "title": title,
                "content": content,
                "method": method,
            })

            print(f"[{len(results)}/{config['max_pages']}] ({depth}) {title[:80]} [{method}]")

            # Extract internal links for crawling
            if depth < config["max_depth"] and len(results) < config["max_pages"]:
                internal = parse_internal_links(html, base_url)
                # Clean seen URLs so newly discovered links aren't duplicates
                internal -= seen_urls
                internal = {u for u in internal if should_crawl(u, config)}
                # Prioritize pages with deeper paths (more content-rich), limit per page
                sorted_links = sorted(internal, key=lambda u: u.count('/'), reverse=True)
                for link in sorted_links[:30]:
                    pages_left.append({"url": link, "depth": depth + 1})
                if internal:
                    print(f"  +{len(internal)} links found, queued {min(30, len(internal))}")

            time.sleep(min_delay)

        except httpx.HTTPError as e:
            print(f"  Error: {url} - {e}")
            continue
        except Exception as e:
            print(f"  Error: {url} - {e}")
            continue

    client.close()
    return results


def merge_documents(results: list[dict], start_url: str) -> tuple[str, str]:
    """Merge all crawled documents into a single markdown file."""
    parts = []
    base_host = urlparse(start_url).netloc

    # Header
    parts.append(f"# Documentation Knowledge Base\n")
    parts.append(f"Source: {start_url}\n")
    parts.append(f"Host: {base_host}\n")
    parts.append(f"Generated: {datetime.now().strftime('%Y-%m-%d %H:%M:%S')}\n")
    parts.append(f"Pages Crawled: {len(results)}\n")
    parts.append(f"Extraction: trafilatura (primary), markdownify (fallback)\n")
    parts.append("---\n\n")

    # Table of contents
    parts.append("## Table of Contents\n\n")
    for i, doc in enumerate(results, 1):
        title_safe = re.sub(r'[^\w\s]', '', doc["title"]).strip()
        anchor = title_safe.lower().replace(" ", "-")[:50]
        parts.append(f"{i}. [{doc['title']}](#{anchor}) - {doc['url']}\n")
    parts.append("\n---\n\n")

    # Documents
    for i, doc in enumerate(results, 1):
        parts.append(f"\n{'='*72}\n")
        parts.append(f"## Page {i}/{len(results)}: {doc['title']}\n\n")
        parts.append(f"*URL: {doc['url']}*\n\n")
        parts.append(f"*Extraction: {doc['method']}*\n\n")
        parts.append(f"---\n\n")
        parts.append(f"{doc['content']}\n\n")
        parts.append(f"{'='*72}\n\n")

    # LLM-optimized section
    merged = "".join(parts)

    # LLM-optimized version
    llm_parts = []
    llm_parts.append("## SYSTEM INSTRUCTION\n")
    llm_parts.append("You are analyzing technical documentation. Follow these rules:\n\n")
    llm_parts.append("1. **Grounding**: Only use information from the documents below\n")
    llm_parts.append("2. **Citations**: Reference the page number (e.g., [Page X])\n")
    llm_parts.append("3. **Accuracy**: If unsure, say you don't know\n")
    llm_parts.append("4. **Code blocks**: Preserve code exactly as shown\n\n")
    llm_parts.append("## DOCUMENTS\n\n")

    for i, doc in enumerate(results, 1):
        llm_parts.append(f"## Page {i}: {doc['title']}\n\n")
        llm_parts.append(f"*Source: {doc['url']}*\n\n")
        llm_parts.append(f"{doc['content']}\n\n")
        llm_parts.append("---\n\n")

    llm_parts.append("## QUESTION\n")
    llm_optimized = "".join(llm_parts)

    return merged, llm_optimized


def main():
    config = get_config()

    start_url = config["url"].rstrip("/")
    output_dir = Path(config["output_dir"])
    output_dir.mkdir(parents=True, exist_ok=True)

    print(f"\nCrawling: {start_url}")
    print(f"Max pages: {config['max_pages']}")
    print(f"Max depth: {config['max_depth']}")
    if config.get("include_paths"):
        print(f"Include paths: {config['include_paths']}")
    print(f"\n")

    crawl_start = time.time()
    results = crawl_site(start_url, config)
    crawl_time = time.time() - crawl_start

    if not results:
        print("\nERROR: No content was extracted.", file=sys.stderr)
        sys.exit(1)

    print(f"\n\nCrawl completed in {crawl_time:.1f}s - {len(results)} pages fetched\n")

    # Save individual pages
    print("Saving individual pages...")
    for i, doc in enumerate(results, 1):
        page_path = save_doc(i, doc["title"], doc["content"], output_dir)
    print(f"  Saved {len(results)} files to {output_dir}/\n")

    # Merge everything
    print("Merging documents...")
    merged, llm_optimized = merge_documents(results, start_url)

    merged_path = output_dir / "merged.md"
    with open(merged_path, "w", encoding="utf-8") as f:
        f.write(merged)
    print(f"  Created: {merged_path} ({len(merged)} chars, {len(merged)/1024:.1f} KB)")

    llm_path = output_dir / "llm.md"
    with open(llm_path, "w", encoding="utf-8") as f:
        f.write(llm_optimized)
    print(f"  Created: {llm_path} ({len(llm_optimized)} chars, {len(llm_optimized)/1024:.1f} KB)")

    # Apply lossless compression
    print("\nApplying lossless compression...")
    compressed = re.sub(r'\n{4,}', '\n\n\n', merged)
    compressed = re.sub(r' +$', '', compressed, flags=re.MULTILINE)
    compressed = re.sub(r'={10,}', '=' * 72, compressed)
    compressed_path = output_dir / "merged_compressed.md"
    with open(compressed_path, "w", encoding="utf-8") as f:
        f.write(compressed)
    reduction = (1 - len(compressed) / len(merged)) * 100 if len(merged) > 0 else 0
    print(f"  Created: {compressed_path} ({len(compressed)} chars, {len(compressed)/1024:.1f} KB, {reduction:.1f}% reduction)")

    print(f"\n{'='*60}")
    print(f"PROCESSING SUMMARY")
    print(f"{'='*60}")
    print(f"  Pages crawled: {len(results)}")
    print(f"  Crawl time: {crawl_time:.1f}s")
    print(f"  Total chars: {len(merged):,}")
    print(f"\n  Output files:")
    print(f"    {merged_path} (merged)")
    print(f"    {llm_path} (LLM-optimized)")
    print(f"    {compressed_path} (lossless compressed)")
    print(f"    {output_dir}/page_XXXX_*.md (individual pages)")


if __name__ == "__main__":
    main()
