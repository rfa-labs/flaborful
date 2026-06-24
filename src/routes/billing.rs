use axum::{
    extract::{Path, State},
    routing::{get, post},
    Json, Router,
};
use uuid::Uuid;

use crate::services::billing::{self as svc, Invoice, Payout};
use crate::{
    auth_extractor::AuthUser,
    errors::{AppError, AppResult},
    AppState,
};

pub fn router() -> Router<AppState> {
    Router::new()
        .route("/generate", post(generate_all))
        .route("/job/:job_id", get(list_for_job))
        .route("/company/:company_id", get(list_for_company))
        .route("/professional/:professional_id/payouts", get(list_payouts))
        .route("/:invoice_id/pay", post(mark_paid))
}

async fn generate_all(
    State(state): State<AppState>,
    _auth: AuthUser,
) -> AppResult<Json<Vec<Invoice>>> {
    let invoices = svc::generate_all_weekly(&state.db)
        .await
        .map_err(AppError::Internal)?;
    Ok(Json(invoices))
}

async fn list_for_job(
    State(state): State<AppState>,
    _auth: AuthUser,
    Path(job_id): Path<Uuid>,
) -> AppResult<Json<Vec<Invoice>>> {
    let invoices = svc::list_invoices_for_job(&state.db, job_id)
        .await
        .map_err(AppError::Internal)?;
    Ok(Json(invoices))
}

async fn list_for_company(
    State(state): State<AppState>,
    _auth: AuthUser,
    Path(company_id): Path<Uuid>,
) -> AppResult<Json<Vec<Invoice>>> {
    let invoices = svc::list_invoices_for_company(&state.db, company_id)
        .await
        .map_err(AppError::Internal)?;
    Ok(Json(invoices))
}

async fn list_payouts(
    State(state): State<AppState>,
    _auth: AuthUser,
    Path(professional_id): Path<Uuid>,
) -> AppResult<Json<Vec<Payout>>> {
    let payouts = svc::list_payouts_for_professional(&state.db, professional_id)
        .await
        .map_err(AppError::Internal)?;
    Ok(Json(payouts))
}

async fn mark_paid(
    State(state): State<AppState>,
    _auth: AuthUser,
    Path(invoice_id): Path<Uuid>,
) -> AppResult<Json<Payout>> {
    let payout = svc::mark_paid(&state.db, invoice_id)
        .await
        .map_err(AppError::Internal)?;
    Ok(Json(payout))
}
