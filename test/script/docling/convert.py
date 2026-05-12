#!/usr/bin/env python3
import sys
import os
import re
import json
from docling.document_converter import DocumentConverter
from pathlib import Path
from datetime import datetime
from typing import Optional
import time

def get_input_files(input_dir: Path):
    """Get all compatible files from input directory and subdirectories."""
    supported_ext = [".pdf", ".docx", ".pptx", ".html", ".htm", ".md", ".txt"]
    files = []
    for item in input_dir.rglob("*"):
        if item.is_file() and item.suffix.lower() in supported_ext:
            files.append(item)
    return sorted(files)

def convert_files(files: list[Path], base_url: str, api_key: str, model: str) -> list[dict]:
    """Convert all files using docling with remote VLM."""
    os.environ["DOCLING_API_URL"] = base_url
    os.environ["DOCLING_API_KEY"] = api_key
    os.environ["DOCLING_API_MODEL"] = model
    os.environ["DOCLING_ENABLE_VISION"] = "true"
    
    converter = DocumentConverter()
    
    converted_docs = []

    for i, file_path in enumerate(files, 1):
        print(f"[{i}/{len(files)}] Processing: {file_path.name}")
        try:
            result = converter.convert(file_path)
            content = result.document.export_to_markdown()
            
            # Post-process formulas
            content = post_process_formulas(content)
            
            converted_docs.append({
                "name": file_path.name,
                "content": content,
                "size_mb": file_path.stat().st_size / (1024 * 1024),
                "path": str(file_path)
            })
            print(f"  -> {len(content)} chars extracted")
        except Exception as e:
            print(f"  ERROR: {e}", file=sys.stderr)

    return converted_docs

def generate_summary(docs: list[dict]) -> str:
    """Generate a summary/index for all documents."""
    summary = []
    summary.append("# Document Knowledge Base\n")
    summary.append("---\n\n")
    summary.append("## Summary\n\n")
    summary.append(f"**Generated:** {datetime.now().strftime('%Y-%m-%d %H:%M:%S')}\n\n")
    summary.append(f"**Total Documents:** {len(docs)}\n\n")
    
    total_size = sum(d["size_mb"] for d in docs)
    summary.append(f"**Total Size:** {total_size:.2f} MB\n\n")
    
    summary.append("---\n\n")
    summary.append("## Table of Contents\n\n")
    
    for i, doc in enumerate(docs, 1):
        summary.append(f"{i}. [{doc['name']}](#{doc['name'].replace('.', '_').lower()})\n")
    
    summary.append("\n---\n\n")
    summary.append("## Documents\n\n")
    
    return "".join(summary)

def build_markdown(docs: list[dict]) -> str:
    """Build merged markdown with summary and clear document boundaries."""
    markdown = generate_summary(docs)
    
    for doc in docs:
        markdown += f"### document: `{doc['name']}`\n\n"
        markdown += doc["content"]
        markdown += "\n\n" + "=" * 72 + "\n\n"
    
    return markdown


def generate_knowledge_index(docs: list[dict], base_url: str, api_key: str, model: str) -> str:
    """Use LLM to generate a knowledge index and format it as markdown section."""
    
    # Prepare document overview for summarization
    doc_overview = []
    for i, doc in enumerate(docs, 1):
        preview = doc['content'][:500].replace('\n', ' ').strip()
        doc_overview.append(f"{i}. {doc['name']} ({doc['size_mb']:.2f} MB)\n   Preview: {preview}...\n")
    
    overview_text = "\n".join(doc_overview)
    
    # Prompt for index generation
    summary_prompt = f"""You are creating a knowledge base index. Generate a CONCISE summary (max 500 words) that helps users quickly understand what information is available.

## Task
Create a searchable index/overview that includes:
1. Main topics covered across all documents
2. Key entities, concepts, or technical terms
3. Document categories or themes
4. Important dates or version information if present

## Documents Overview
{overview_text}

## Output Format
Return ONLY valid JSON with this structure:
{{
  "knowledge_domain": "brief description of overall domain",
  "key_topics": ["topic1", "topic2", "topic3"],
  "document_summaries": [
    {{"name": "doc1.pdf", "main_points": ["point1", "point2"]}}
  ],
  "key_entities": ["entity1", "entity2"],
  "search_hints": ["hint1", "hint2"]
}}

Do not add any text outside the JSON. Be concise."""
    
    # Direct HTTP call
    try:
        import urllib.request
        
        api_url = base_url
        if not api_url.rstrip('/').endswith('/chat/completions'):
            api_url = api_url.rstrip('/') + '/chat/completions'
        
        payload = {
            "model": model,
            "messages": [
                {"role": "system", "content": "You are a helpful assistant that returns valid JSON only."},
                {"role": "user", "content": summary_prompt}
            ],
            "max_tokens": 2000,
            "temperature": 0.3,
        }
        
        data = json.dumps(payload).encode()
        
        req = urllib.request.Request(
            api_url,
            data=data,
            headers={
                "Content-Type": "application/json",
                "Authorization": f"Bearer {api_key}"
            }
        )
        
        with urllib.request.urlopen(req, timeout=120) as resp:
            response = json.loads(resp.read().decode())
            content = response["choices"][0]["message"]["content"].strip()
        
        # Strip markdown code fences if present
        if content.startswith('```'):
            content = re.sub(r'^```(?:json)?\s*', '', content, flags=re.MULTILINE)
            content = re.sub(r'\s*```$', '', content)
        
        summary_json = json.loads(content)
        
        return format_summary_as_markdown(summary_json)
        
    except Exception as e:
        print(f"    Index generation failed ({e}), using fallback")
    
    return generate_fallback_index(docs)


def format_summary_as_markdown(summary_json: dict) -> str:
    """Convert JSON summary to formatted markdown index section."""
    
    md = []
    md.append("## KNOWLEDGE INDEX\n\n")
    
    # Domain
    if "knowledge_domain" in summary_json:
        md.append(f"**Domain:** {summary_json['knowledge_domain']}\n\n")
    
    # Key topics
    if "key_topics" in summary_json and summary_json['key_topics']:
        md.append("### Key Topics Covered\n")
        for topic in summary_json['key_topics'][:10]:
            md.append(f"- {topic}\n")
        md.append("\n")
    
    # Document summaries
    if "document_summaries" in summary_json:
        md.append("### Document Overview\n")
        for doc in summary_json['document_summaries'][:5]:
            md.append(f"**{doc.get('name', 'Unknown')}**\n")
            if 'main_points' in doc:
                for point in doc['main_points'][:3]:
                    md.append(f"  - {point}\n")
            md.append("\n")
    
    # Key entities
    if "key_entities" in summary_json and summary_json['key_entities']:
        md.append("### Key Entities & Concepts\n")
        for entity in summary_json['key_entities'][:15]:
            md.append(f"- {entity}\n")
        md.append("\n")
    
    # Search hints
    if "search_hints" in summary_json and summary_json['search_hints']:
        md.append("### Quick Search Hints\n")
        for hint in summary_json['search_hints'][:8]:
            md.append(f"- {hint}\n")
        md.append("\n")
    
    md.append("---\n\n")
    
    return "".join(md)


def generate_fallback_index(docs: list[dict]) -> str:
    """Fallback index generator if LLM is unavailable."""
    
    md = []
    md.append("## KNOWLEDGE INDEX\n\n")
    md.append(f"**Total Documents:** {len(docs)}\n\n")
    
    # Extract file types
    ext_count = {}
    for doc in docs:
        ext = Path(doc['name']).suffix
        ext_count[ext] = ext_count.get(ext, 0) + 1
    
    md.append("### Document Types\n")
    for ext, count in ext_count.items():
        md.append(f"- {ext}: {count} document(s)\n")
    md.append("\n")
    
    # List documents
    md.append("### Available Documents\n")
    for i, doc in enumerate(docs, 1):
        md.append(f"{i}. **{doc['name']}** ({doc['size_mb']:.2f} MB)\n")
    md.append("\n")
    
    md.append("---\n\n")
    
    return "".join(md)


def build_llm_optimized_markdown(docs: list[dict]) -> str:
    """Build markdown specifically optimized for LLM prompts (system + documents)."""
    output = []
    
    # System instruction
    output.append("## SYSTEM INSTRUCTION\n")
    output.append("You are analyzing technical documents. Follow these rules:\n")
    output.append("\n")
    output.append("1. **Grounding**: Only use information from the documents below\n")
    output.append("2. **Missing content**: If formulas appear as placeholders, infer meaning from surrounding text\n")
    output.append("3. **Images**: Treat diagram descriptions as contextual hints\n")
    output.append("4. **Code**: Explain code blocks when relevant\n")
    output.append("5. **Citations**: Always cite both filename AND section number\n")
    output.append("\n")
    output.append("   **Correct examples:**\n")
    output.append("   - `[bitcoin.pdf, Section 4 - Proof-of-Work]`\n")
    output.append("   - `[bitcoin.pdf, Section 11]`\n")
    output.append("   - `[bitcoin.pdf, Section 5, Step 3]`\n")
    output.append("\n")
    output.append("   **Incorrect examples:**\n")
    output.append("   - `[bitcoin.pdf]` (missing section)\n")
    output.append("   - `Section 4` (missing filename)\n")
    output.append("\n")
    
    # Documents section
    output.append("## DOCUMENTS\n\n")
    
    # Document content with LLM enhancements
    llm_optimized = build_llm_enhanced_content(docs)
    output.append(llm_optimized)
    
    # Add QUESTION placeholder for user
    output.append("\n## QUESTION\n")
    
    return "".join(output)


def build_index_markdown(docs: list[dict], base_url: str = None, api_key: str = None, model: str = None) -> str:
    """Build a complete LLM prompt file with system instruction, knowledge index, and documents."""
    
    output = []
    
    # 1. System instruction
    output.append("## SYSTEM INSTRUCTION\n")
    output.append("You are analyzing technical documents. Follow these rules:\n")
    output.append("\n")
    output.append("1. **Grounding**: Only use information from the documents below\n")
    output.append("2. **Missing content**: If formulas appear as placeholders, infer meaning from surrounding text\n")
    output.append("3. **Images**: Treat diagram descriptions as contextual hints\n")
    output.append("4. **Code**: Explain code blocks when relevant\n")
    output.append("5. **Citations**: Always cite both filename AND section number\n")
    output.append("\n")
    output.append("   **Correct examples:**\n")
    output.append("   - `[bitcoin.pdf, Section 4 - Proof-of-Work]`\n")
    output.append("   - `[bitcoin.pdf, Section 11]`\n")
    output.append("   - `[bitcoin.pdf, Section 5, Step 3]`\n")
    output.append("\n")
    output.append("   **Incorrect examples:**\n")
    output.append("   - `[bitcoin.pdf]` (missing section)\n")
    output.append("   - `Section 4` (missing filename)\n")
    output.append("\n")
    
    # 2. Knowledge index
    if base_url and api_key and model:
        print("  Generating knowledge base index...")
        index_content = generate_knowledge_index(docs, base_url, api_key, model)
        output.append(index_content)
    else:
        output.append("## KNOWLEDGE INDEX\n")
        output.append(f"**Total Documents:** {len(docs)}\n\n")
        for i, doc in enumerate(docs, 1):
            output.append(f"{i}. {doc['name']}\n")
        output.append("\n---\n\n")
    
    # 3. Documents
    output.append("## DOCUMENTS\n\n")
    output.append(build_llm_enhanced_content(docs))
    
    # 4. QUESTION placeholder
    output.append("\n## QUESTION\n")
    
    return "".join(output)


def build_llm_enhanced_content(docs: list[dict]) -> str:
    """Build document content with LLM-specific processing."""
    output = []
    
    for doc in docs:
        output.append(f"## Document: {doc['name']}\n\n")
        
        content = doc['content']
        
        # Fix missing formulas
        content = re.sub(
            r'<!-- formula-not-decoded -->',
            '[Mathematical formula from original document]',
            content
        )
        
        # Add context for images
        content = re.sub(
            r'<!-- image -->',
            '[Diagram: See original document for visual representation]',
            content
        )
        
        output.append(content)
        output.append("\n---\n\n")
    
    return "".join(output)

def post_process_formulas(content: str) -> str:
    """Convert formula placeholders to proper LaTeX."""
    formula_patterns = [
        (r'probability\s+(\w+)\s+=\s+(\d+(?:\.\d+)?)', '$$P(\\1) = \\2$$'),
        (r'q\s*=\s*probability.*?attacker.*?finds', '$$q = P(\\\\text{attacker finds block})$$'),
        (r'p\s*=\s*probability.*?honest.*?finds', '$$p = P(\\\\text{honest node finds block})$$'),
        (r'q_z\s*=\s*(?:probability|\(q/p\)\^z)', '$$q_z = \\\\begin{cases} 1 & \\\\text{if } p \\\\leq q \\\\\\\\ (q/p)^z & \\\\text{if } p > q \\\\end{cases}$$'),
    ]
    for pattern, replacement in formula_patterns:
        content = re.sub(pattern, replacement, content, flags=re.IGNORECASE | re.DOTALL)
    content = re.sub(r'`([^`]+)`\s*[=]\s*([^=\n]+)', '$$\\1 = \\2$$', content)
    return content


def lossless_compress(markdown: str) -> str:
    """Apply safe, lossless compression to markdown."""
    compressed = re.sub(r'\n{3,}', '\n\n', markdown)
    compressed = re.sub(r' +$', '', compressed, flags=re.MULTILINE)
    lines = compressed.split('\n')
    processed_lines = []
    in_code_block = False
    for line in lines:
        if line.strip().startswith('```'):
            in_code_block = not in_code_block
            processed_lines.append(line)
        elif in_code_block:
            processed_lines.append(line)
        else:
            processed_lines.append(re.sub(r' {2,}', ' ', line))
    compressed = '\n'.join(processed_lines)
    compressed = re.sub(r'={10,}', '=' * 72, compressed)
    compressed = re.sub(r'\n(#+\s)', '\n\n\\1', compressed)
    compressed = compressed.replace('<!-- image -->', '*[IMG]*')
    return compressed

def validate_optimization(original: str, optimized: str) -> bool:
    """Validate that optimization didn't corrupt the content."""
    if not optimized or len(optimized) < len(original) * 0.1:
        print(f"    Validation failed: Output too small ({len(optimized)} vs {len(original)} chars)")
        return False
    if len(optimized) > len(original) * 1.5:
        print(f"    Validation failed: Output too large ({len(optimized)} vs {len(original)} chars)")
        return False
    truncation_markers = ['{optimized', '```json', '```python', 'Here is', "I've"]
    for marker in truncation_markers:
        if marker in optimized[:200]:
            print(f"    Validation failed: Found truncation marker '{marker}'")
            return False
    original_headers = set(re.findall(r'^##+\s+.*$', original, re.MULTILINE))
    optimized_headers = set(re.findall(r'^##+\s+.*$', optimized, re.MULTILINE))
    if len(optimized_headers) < len(original_headers) * 0.5:
        print(f"    Validation failed: Lost too many headers ({len(optimized_headers)} vs {len(original_headers)})")
        return False
    return True

def parse_args():
    """Parse key=value arguments for config."""
    config = {}
    for arg in sys.argv[1:]:
        if "=" in arg:
            key, value = arg.split("=", 1)
            key = key.lstrip("-").replace("-", "_")
            if value != "":
                config[key] = value
    return config

def get_api_key() -> str:
    """Get API key from environment."""
    return os.environ.get("API_KEY", "")

def use_info():
    """Print usage information."""
    print("Usage: docling-pdf2md base_url=... api_key=... model=...", file=sys.stderr)
    print("", file=sys.stderr)
    print("Required config arguments:", file=sys.stderr)
    print("  base_url   - LLM endpoint URL (e.g., https://model.inferx.net/.../v1)", file=sys.stderr)
    print("  api_key    - API key for authentication", file=sys.stderr)
    print("  model      - Model name without provider (e.g., Qwen3-Coder-Next-FP8)", file=sys.stderr)
    print("", file=sys.stderr)
    print("Optional environment variables:", file=sys.stderr)
    print("  USE_DSPY=true  - Enable DSPy optimization (EXPERIMENTAL, default: false)", file=sys.stderr)
    print("  API_KEY        - Alternative way to provide API key", file=sys.stderr)
    print("", file=sys.stderr)
    print("NOTE: Lossless compression is used by default. DSPy optimization is experimental", file=sys.stderr)
    print("      and may corrupt output. Only enable if you understand the risks.", file=sys.stderr)
    sys.exit(1)

def main():
    input_dir = Path(os.environ.get("INPUT_DIR", "/input"))
    output_dir = Path(os.environ.get("OUTPUT_DIR", "/output"))
    docling_output = output_dir / "merged.md"
    optimized_output = output_dir / "optimized.md"
    index_output = output_dir / "index.md"

    config = parse_args()

    base_url = config.get("base_url", "https://model.inferx.net/funccall/tn-a3t79iogb2/endpoints/Qwen3-Coder-Next-FP8/v1")
    api_key_arg = config.get("api_key")
    api_key = get_api_key() if api_key_arg is None or api_key_arg == "" else api_key_arg
    model = config.get("model", "Qwen/Qwen3-Coder-Next-FP8")
    
    use_dspy = os.environ.get("USE_DSPY", "false").lower() == "true"

    if not base_url or not api_key or not model:
        use_info()

    output_dir.mkdir(parents=True, exist_ok=True)

    if not input_dir.exists():
        print(f"ERROR: Input directory {input_dir} does not exist", file=sys.stderr)
        sys.exit(1)

    files = get_input_files(input_dir)
    if not files:
        print("ERROR: No compatible files found in /input", file=sys.stderr)
        sys.exit(1)

    print(f"Found {len(files)} files to convert.")
    print(f"Using model: {model}")
    print(f"Base URL: {base_url}")
    
    if use_dspy:
        print(f"⚠️  DSPy optimization: ENABLED (experimental, may corrupt output)")
    else:
        print(f"✓ Lossless compression: ENABLED (safe, recommended)")
    
    docling_start = time.time()
    docs = convert_files(files, base_url, api_key, model)
    docling_end = time.time()
    
    if not docs:
        print("ERROR: No files were successfully converted", file=sys.stderr)
        sys.exit(1)

    markdown = build_markdown(docs)

    # llm.md: system instruction + documents (no index)
    llm_optimized = build_llm_optimized_markdown(docs)
    
    # index.md: complete LLM prompt with system + index + documents
    index_markdown = build_index_markdown(docs, base_url, api_key, model)
    
    with open(docling_output, "w", encoding="utf-8") as f:
        f.write(markdown)
    
    llm_output = output_dir / "llm.md"
    with open(llm_output, "w", encoding="utf-8") as f:
        f.write(llm_optimized)
    
    with open(index_output, "w", encoding="utf-8") as f:
        f.write(index_markdown)
    
    docling_chars = len(markdown)
    llm_chars = len(llm_optimized)
    print(f"\n✓ Docling: Created {docling_output} ({docling_chars} chars, {docling_chars/1024:.1f} KB)")
    print(f"  LLM-optimized: {llm_output} ({llm_chars} chars, {llm_chars/1024:.1f} KB)")
    print(f"  Index prompt: {index_output} ({len(index_markdown)} chars)")
    print(f"  Processing time: {docling_end - docling_start:.2f} seconds")

    opt_start = time.time()
    print("\nOptimizing for KV cache...")
    
    optimized = None
    optimization_method = "Lossless Compression"
    
    if use_dspy:
        print("  Attempting DSPy optimization...")
        try:
            import dspy
            from dspy.signatures import Signature
            
            lm = dspy.LM(f"openai/{model}", 
                         api_base=base_url, 
                         api_key=api_key, 
                         max_tokens=60000, 
                         stop=None, 
                         temperature=0.0,
                         cache=False)
            dspy.configure(lm=lm)
            
            print(f"    DSPy {dspy.__version__} configured")
            
            class OptimizeMarkdown(Signature):
                """Remove redundant whitespace while preserving ALL content."""
                raw_markdown: str = dspy.InputField()
                optimized_markdown: str = dspy.OutputField()
            
            optimizer = dspy.Predict(OptimizeMarkdown)
            
            MAX_CHUNK_SIZE = 8000
            chunks = []
            for i in range(0, len(markdown), MAX_CHUNK_SIZE):
                chunks.append(markdown[i:i+MAX_CHUNK_SIZE])
            
            if len(chunks) > 1:
                print(f"    Splitting into {len(chunks)} chunks...")
                optimized_chunks = []
                for i, chunk in enumerate(chunks, 1):
                    print(f"      Chunk {i}/{len(chunks)} ({len(chunk)} chars)...")
                    try:
                        result = optimizer(raw_markdown=chunk)
                        chunk_optimized = result.optimized_markdown
                        
                        if validate_optimization(chunk, chunk_optimized):
                            optimized_chunks.append(chunk_optimized)
                        else:
                            print(f"        Chunk {i} validation failed, using original")
                            optimized_chunks.append(chunk)
                    except Exception as e:
                        print(f"        Error on chunk {i}: {e}")
                        optimized_chunks.append(chunk)
                    time.sleep(0.3)
                
                optimized = "".join(optimized_chunks)
            else:
                print(f"    Processing single chunk ({len(markdown)} chars)...")
                result = optimizer(raw_markdown=markdown)
                optimized = result.optimized_markdown
            
            if optimized and validate_optimization(markdown, optimized):
                optimization_method = "DSPy"
                print(f"    ✓ DSPy optimization successful")
            else:
                print(f"    ✗ DSPy optimization failed validation, falling back to lossless compression")
                optimized = None
                use_dspy = False
            
        except ImportError:
            print("    ✗ DSPy not available")
            use_dspy = False
        except Exception as e:
            print(f"    ✗ DSPy error: {e}")
            use_dspy = False
    
    if not use_dspy or optimized is None:
        print("  Applying lossless compression...")
        optimized = lossless_compress(markdown)
        optimization_method = "Lossless Compression"
    
    opt_end = time.time()
    
    if optimized:
        optimized_chars = len(optimized)
        reduction = (1 - optimized_chars / len(markdown)) * 100 if len(markdown) > 0 else 0
        
        with open(optimized_output, "w", encoding="utf-8") as f:
            f.write(optimized)
        
        print(f"\n✓ Optimization complete ({optimization_method}):")
        print(f"    Created: {optimized_output}")
        print(f"    Original: {len(markdown):,} chars ({len(markdown)/1024:.1f} KB)")
        print(f"    Optimized: {optimized_chars:,} chars ({optimized_chars/1024:.1f} KB)")
        print(f"    Reduction: {reduction:.1f}%")
        print(f"    Processing time: {opt_end - opt_start:.2f} seconds")
    else:
        print("  ✗ Optimization failed, using original file")
        with open(optimized_output, "w", encoding="utf-8") as f:
            f.write(markdown)
        optimized_chars = len(markdown)

    print(f"\n{'='*60}")
    print("PROCESSING SUMMARY")
    print(f"{'='*60}")
    print(f"✓ Docling: {docling_end - docling_start:.2f}s | {len(markdown):,} chars")
    print(f"✓ LLM-optimized: {len(llm_optimized):,} chars")
    print(f"✓ Optimization ({optimization_method}): {opt_end - opt_start:.2f}s | {optimized_chars:,} chars")
    print(f"\nOutput files:")
    print(f"  - {docling_output} (original)")
    print(f"  - {optimized_output} (optimized)")
    print(f"  - {llm_output} (LLM-optimized)")
    print(f"  - {index_output} (knowledge index)")

if __name__ == "__main__":
    main()
