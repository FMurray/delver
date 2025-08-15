use serde::{Deserialize, Serialize};
use uuid::Uuid;

#[cfg(feature = "ssr")]
use {
    sqlx::{Row, SqlitePool},
    std::collections::HashMap,
};

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct DocumentPage {
    pub page_index: usize,
    pub image_data: Vec<u8>, // RGBA image data
    pub width: f32,
    pub height: f32,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct PdfDocument {
    pub id: Uuid,
    pub name: String,
    pub total_pages: usize,
    pub created_at: chrono::DateTime<chrono::Utc>,
    pub pdf_data: Option<Vec<u8>>, // Raw PDF bytes for server-side processing
}

impl PdfDocument {
    pub fn new(name: String) -> Self {
        Self {
            id: Uuid::new_v4(),
            name,
            total_pages: 0,
            created_at: chrono::Utc::now(),
            pdf_data: None,
        }
    }
}

// Server-side database operations
#[cfg(feature = "ssr")]
impl PdfDocument {
    pub async fn create(
        pool: &SqlitePool,
        name: String,
        pdf_data: Vec<u8>,
        total_pages: usize,
    ) -> Result<PdfDocument, sqlx::Error> {
        let id = Uuid::new_v4();
        let created_at = chrono::Utc::now();

        sqlx::query(
            "INSERT INTO documents (id, name, total_pages, pdf_data, created_at) VALUES (?, ?, ?, ?, ?)"
        )
        .bind(id.to_string())
        .bind(&name)
        .bind(total_pages as i32)
        .bind(&pdf_data)
        .bind(created_at)
        .execute(pool)
        .await?;

        Ok(PdfDocument {
            id,
            name,
            total_pages,
            created_at,
            pdf_data: Some(pdf_data),
        })
    }

    pub async fn get_by_id(
        pool: &SqlitePool,
        id: Uuid,
    ) -> Result<Option<PdfDocument>, sqlx::Error> {
        let result = sqlx::query(
            "SELECT id, name, total_pages, pdf_data, created_at FROM documents WHERE id = ?",
        )
        .bind(id.to_string())
        .fetch_optional(pool)
        .await?;

        if let Some(row) = result {
            let id = Uuid::parse_str(&row.get::<String, _>("id"))
                .map_err(|e| sqlx::Error::Protocol(format!("Invalid UUID: {}", e)))?;
            Ok(Some(PdfDocument {
                id,
                name: row.get("name"),
                total_pages: row.get::<i32, _>("total_pages") as usize,
                created_at: row.get("created_at"),
                pdf_data: Some(row.get("pdf_data")),
            }))
        } else {
            Ok(None)
        }
    }

    pub async fn get_all(pool: &SqlitePool) -> Result<Vec<PdfDocument>, sqlx::Error> {
        let rows = sqlx::query(
            "SELECT id, name, total_pages, created_at FROM documents ORDER BY created_at DESC",
        )
        .fetch_all(pool)
        .await?;

        let mut documents = Vec::new();
        for row in rows {
            let id = Uuid::parse_str(&row.get::<String, _>("id"))
                .map_err(|e| sqlx::Error::Protocol(format!("Invalid UUID: {}", e)))?;
            documents.push(PdfDocument {
                id,
                name: row.get("name"),
                total_pages: row.get::<i32, _>("total_pages") as usize,
                created_at: row.get("created_at"),
                pdf_data: None, // Don't load PDF data for list view
            });
        }
        Ok(documents)
    }

    pub async fn delete(pool: &SqlitePool, id: Uuid) -> Result<bool, sqlx::Error> {
        // Delete associated pages first
        sqlx::query("DELETE FROM document_pages WHERE document_id = ?")
            .bind(id.to_string())
            .execute(pool)
            .await?;

        // Delete the document
        let result = sqlx::query("DELETE FROM documents WHERE id = ?")
            .bind(id.to_string())
            .execute(pool)
            .await?;

        Ok(result.rows_affected() > 0)
    }
}

#[cfg(feature = "ssr")]
impl DocumentPage {
    pub async fn create(
        pool: &SqlitePool,
        document_id: Uuid,
        page_index: usize,
        image_data: Vec<u8>,
        width: f32,
        height: f32,
    ) -> Result<(), sqlx::Error> {
        sqlx::query(
            "INSERT OR REPLACE INTO document_pages (document_id, page_index, image_data, width, height) VALUES (?, ?, ?, ?, ?)"
        )
        .bind(document_id.to_string())
        .bind(page_index as i32)
        .bind(&image_data)
        .bind(width)
        .bind(height)
        .execute(pool)
        .await?;

        Ok(())
    }

    pub async fn get_by_document_and_page(
        pool: &SqlitePool,
        document_id: Uuid,
        page_index: usize,
    ) -> Result<Option<DocumentPage>, sqlx::Error> {
        let result = sqlx::query(
            "SELECT page_index, image_data, width, height FROM document_pages WHERE document_id = ? AND page_index = ?"
        )
        .bind(document_id.to_string())
        .bind(page_index as i32)
        .fetch_optional(pool)
        .await?;

        if let Some(row) = result {
            Ok(Some(DocumentPage {
                page_index: row.get::<i32, _>("page_index") as usize,
                image_data: row.get("image_data"),
                width: row.get("width"),
                height: row.get("height"),
            }))
        } else {
            Ok(None)
        }
    }

    pub async fn get_all_for_document(
        pool: &SqlitePool,
        document_id: Uuid,
    ) -> Result<HashMap<usize, DocumentPage>, sqlx::Error> {
        let rows = sqlx::query(
            "SELECT page_index, image_data, width, height FROM document_pages WHERE document_id = ? ORDER BY page_index"
        )
        .bind(document_id.to_string())
        .fetch_all(pool)
        .await?;

        let mut pages = HashMap::new();
        for row in rows {
            let page = DocumentPage {
                page_index: row.get::<i32, _>("page_index") as usize,
                image_data: row.get("image_data"),
                width: row.get("width"),
                height: row.get("height"),
            };
            pages.insert(page.page_index, page);
        }
        Ok(pages)
    }
}

// Database initialization
#[cfg(feature = "ssr")]
pub async fn create_tables(pool: &SqlitePool) -> Result<(), sqlx::Error> {
    // Create documents table
    sqlx::query(
        r#"
        CREATE TABLE IF NOT EXISTS documents (
            id TEXT PRIMARY KEY NOT NULL,
            name TEXT NOT NULL,
            total_pages INTEGER NOT NULL,
            pdf_data BLOB NOT NULL,
            created_at DATETIME NOT NULL
        )
        "#,
    )
    .execute(pool)
    .await?;

    // Create document_pages table
    sqlx::query(
        r#"
        CREATE TABLE IF NOT EXISTS document_pages (
            document_id TEXT NOT NULL,
            page_index INTEGER NOT NULL,
            image_data BLOB NOT NULL,
            width REAL NOT NULL,
            height REAL NOT NULL,
            PRIMARY KEY (document_id, page_index),
            FOREIGN KEY (document_id) REFERENCES documents (id) ON DELETE CASCADE
        )
        "#,
    )
    .execute(pool)
    .await?;

    Ok(())
}

// Connection pool utilities
#[cfg(feature = "ssr")]
pub async fn get_database_pool() -> Result<SqlitePool, sqlx::Error> {
    use sqlx::sqlite::SqliteConnectOptions;
    use std::str::FromStr;

    let database_url =
        std::env::var("DATABASE_URL").unwrap_or_else(|_| "sqlite:delver.db".to_string());

    // Parse the database URL and ensure the file is created if it doesn't exist
    let connect_options = SqliteConnectOptions::from_str(&database_url)?.create_if_missing(true);

    let pool = SqlitePool::connect_with(connect_options).await?;
    create_tables(&pool).await?;
    Ok(pool)
}
