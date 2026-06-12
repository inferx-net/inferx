# Docling PDF to Markdown API

REST API service for converting PDF documents to Markdown using Docling.

## Files

- `Dockerfile` - Builds the container using `docling-pdf2md:latest` base image
- `convert.py` - Flask API with `/convert` endpoint
- `warmup.py` - Pre-downloads OCR models at Docker build time

## Build

```bash
sudo docker build -t docker.io/inferx/inferx_dockling_svc:V0.1.0 .
```

## Run

```bash
# Set API_KEY environment variable
sudo docker run --rm -it --name docling-api -p 8000:8000 \
  -e API_KEY=your-secret-api-key \
  docling-api
```

## API Endpoints

### GET `/`
Get API information

### GET `/health`
Health check endpoint

### POST `/convert`
Convert a PDF document to Markdown.

**Authentication:** Include API key in `Authorization` header as `Bearer <key>`.

**With config via headers:**
```bash
curl -X POST \
  -H "Authorization: Bearer secret123" \
  -H "X-Do-Table-Structure: false" \
  -F "file=@document.pdf" \
  http://localhost:8000/convert > output.md
```

**Available config headers:**
| Header | Default |
|--------|---------|
| `X-Do-Ocr` | `false` |
| `X-Do-Table-Structure` | `false` |
| `X-Do-Picture-Classification` | `false` |
| `X-Do-Picture-Description` | `false` |
| `X-Do-Chart-Extraction` | `false` |
| `X-Do-Code-Enrichment` | `false` |
| `X-Do-Formula-Enrichment` | `false` |

## Example

```bash
# Start service with API key
sudo docker run -it --rm --name inferx_dockling_svc -p 8000:8000 \
  -e API_KEY=secret123 \
  docker.io/inferx/inferx_dockling_svc:V0.1.0

# Convert with authentication
curl -X POST \
  -H "Authorization: Bearer secret123" \
  -F "file=@document.pdf" \
  http://localhost:8000/convert > output.md

# Convert with config
curl -X POST \
  -H "Authorization: Bearer secret123" \
  -H "X-Do-Ocr: true" \
  -F "file=@document.pdf" \
  http://localhost:8000/convert > output.md
```

## Notes

- OCR models are pre-downloaded during Docker build
- Default is fastest (no OCR, no table extraction)
- API key is required for all `/convert` requests
- Use headers to enable specific features
