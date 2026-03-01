#!/bin/bash
# ============================================================
# ferricula.nuts.services → Google Cloud Run deployment
# Run from the site/ directory containing:
#   index.html, Dockerfile, nginx.conf
# ============================================================
# Prerequisites: gcloud CLI installed + authenticated
#   brew install google-cloud-sdk   (or equivalent)
#   gcloud auth login
# ============================================================

set -e

PROJECT_ID="gnosis-459403"
REGION="us-central1"
SERVICE="ferricula-site"
IMAGE="gcr.io/${PROJECT_ID}/${SERVICE}"

echo "==> 1. Setting project"
gcloud config set project $PROJECT_ID

echo "==> 2. Enabling required APIs (one-time)"
gcloud services enable \
  run.googleapis.com \
  cloudbuild.googleapis.com \
  artifactregistry.googleapis.com

echo "==> 3. Building + pushing container via Cloud Build"
gcloud builds submit --tag $IMAGE .

echo "==> 4. Deploying to Cloud Run"
gcloud run deploy $SERVICE \
  --image $IMAGE \
  --region $REGION \
  --platform managed \
  --allow-unauthenticated \
  --port 8080 \
  --memory 128Mi \
  --cpu 1 \
  --min-instances 0 \
  --max-instances 3

echo "==> 5. Mapping custom domain"
gcloud run domain-mappings create \
  --service $SERVICE \
  --domain ferricula.nuts.services \
  --region $REGION

echo ""
echo "============================================================"
echo "DONE. Add DNS records in your domain provider:"
echo ""
echo "Google will show you DNS records. Typically:"
echo ""
echo "  Type   Name          Value"
echo "  ----   ----          -----"
echo "  CNAME  ferricula     ghs.googlehosted.com."
echo ""
echo "SSL cert is automatic. Provisioning takes 15-30 min."
echo "Check status:  gcloud run domain-mappings describe \\"
echo "  --domain ferricula.nuts.services --region $REGION"
echo "============================================================"
