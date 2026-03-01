# ============================================================
# ferricula.nuts.services -> Google Cloud Run deployment
# Run from the site/ directory containing:
#   index.html, Dockerfile, nginx.conf
# ============================================================

param(
    [string]$ProjectId = "gnosis-459403",
    [string]$Region = "us-central1",
    [string]$Service = "ferricula-site",
    [string]$Domain = "ferricula.nuts.services"
)

$Image = "gcr.io/$ProjectId/$Service"

Write-Host "==> 1. Setting project" -ForegroundColor Cyan
gcloud config set project $ProjectId

Write-Host "==> 2. Enabling required APIs (one-time)" -ForegroundColor Cyan
gcloud services enable `
  run.googleapis.com `
  cloudbuild.googleapis.com `
  artifactregistry.googleapis.com

Write-Host "==> 3. Building + pushing container via Cloud Build" -ForegroundColor Cyan
gcloud builds submit --tag $Image .

Write-Host "==> 4. Deploying to Cloud Run" -ForegroundColor Cyan
gcloud run deploy $Service `
  --image $Image `
  --region $Region `
  --platform managed `
  --allow-unauthenticated `
  --port 8080 `
  --memory 128Mi `
  --cpu 1 `
  --min-instances 0 `
  --max-instances 3

Write-Host "==> 5. Mapping custom domain" -ForegroundColor Cyan
gcloud run domain-mappings create `
  --service $Service `
  --domain $Domain `
  --region $Region

Write-Host ""
Write-Host "============================================================" -ForegroundColor Green
Write-Host "DONE. Add DNS records in your domain provider:" -ForegroundColor Green
Write-Host ""
Write-Host "  Type   Name          Value"
Write-Host "  ----   ----          -----"
Write-Host "  CNAME  ferricula     ghs.googlehosted.com."
Write-Host ""
Write-Host "SSL cert is automatic. Provisioning takes 15-30 min."
Write-Host "Check status:" -ForegroundColor Yellow
Write-Host "  gcloud run domain-mappings describe --domain $Domain --region $Region"
Write-Host "============================================================" -ForegroundColor Green
