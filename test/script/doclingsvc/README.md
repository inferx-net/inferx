# Docling PDF to Markdown API

REST API service for converting PDF documents to Markdown using Docling.

## Files

- `Dockerfile` - Builds the container using `docling-pdf2md:latest` base image
- `convert.py` - Flask API with `/convert` endpoint
- `warmup.py` - Pre-downloads OCR models at Docker build time

## Build

```bash
sudo docker build -t docling-api .
```

## Run

```bash
sudo docker run --rm -it --name docling-api -p 8000:8000 docling-api
```

## API Endpoints

### GET `/`
Get API information

### GET `/health`
Health check endpoint

### POST `/convert`
Convert a PDF document to Markdown.

**With config via headers:**
```bash
curl -X POST \
  -H "X-Do-Ocr: true" \
  -H "X-Do-Table-Structure: true" \
  -F "file=@document.pdf" \
  http://localhost:8000/convert > output.md
```

**Available headers:**
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
# Start service
sudo docker run -d --name docling-api -p 8000:8000 docling-api
sleep 5

# Convert with default settings
curl -X POST -F "file=@document.pdf" http://localhost:8000/convert > output.md

# Convert with OCR enabled
curl -X POST -H "X-Do-Ocr: true" -F "file=@document.pdf" http://localhost:8000/convert > output.md

# Convert with tables
curl -X POST -H "X-Do-Table-Structure: true" -F "file=@document.pdf" http://localhost:8000/convert > output.md
```

## Notes

- OCR models are pre-downloaded during Docker build
- Default is fastest (no OCR, no table extraction)
- Use headers to enable specific features
