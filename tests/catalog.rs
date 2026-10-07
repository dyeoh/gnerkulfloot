//! Catalog end to end: staff build products, shoppers browse them.

mod common;

use axum::http::StatusCode;
use common::{admin_token, send, send_bytes};
use serde_json::{Value, json};
use sqlx::PgPool;

/// Creates an active product with one SKU priced in MYR (and optionally more), returns (product_id, sku_id).
async fn product(app: &axum::Router, admin: &str, name: &str, myr: i64, stock: i32) -> (String, String) {
    let p = send(
        app,
        "POST",
        "/v1/admin/products",
        Some(admin),
        Some(json!({"name": name, "status": "active"})),
    )
    .await;
    assert_eq!(p.status, StatusCode::CREATED, "{:?}", p.json);
    let id = p.json["id"].as_str().unwrap().to_owned();
    let code = format!("SKU-{}", &id[id.len() - 8..]);
    let s = send(
        app,
        "POST",
        &format!("/v1/admin/products/{id}/skus"),
        Some(admin),
        Some(json!({
            "code": code, "stock_available": stock,
            "prices": [{"currency": "MYR", "amount": myr}]
        })),
    )
    .await;
    assert_eq!(s.status, StatusCode::CREATED, "{:?}", s.json);
    (id, s.json["id"].as_str().unwrap().to_owned())
}

fn names(res: &Value) -> Vec<&str> {
    res["items"]
        .as_array()
        .unwrap()
        .iter()
        .map(|i| i["name"].as_str().unwrap())
        .collect()
}

#[sqlx::test]
async fn staff_build_a_product_and_shoppers_see_it_per_currency(db: PgPool) {
    let app = common::router(db.clone(), common::config());
    let admin = admin_token(&app, &db).await;

    let cat = send(
        &app,
        "POST",
        "/v1/admin/categories",
        Some(&admin),
        Some(json!({"name": "Baju Kurung"})),
    )
    .await;
    assert_eq!(cat.status, StatusCode::CREATED);
    assert_eq!(cat.json["slug"], "baju-kurung");
    let cat_id = cat.json["id"].as_str().unwrap();

    let p = send(
        &app,
        "POST",
        "/v1/admin/products",
        Some(&admin),
        Some(json!({
            "name": "Baju Kurung Moden (Red)", "description": "Cotton, hand-stitched.",
            "category_ids": [cat_id], "attributes": {"material": "cotton"}
        })),
    )
    .await;
    assert_eq!(p.status, StatusCode::CREATED, "{:?}", p.json);
    assert_eq!(p.json["status"], "draft");
    assert_eq!(p.json["slug"], "baju-kurung-moden-red");
    let pid = p.json["id"].as_str().unwrap().to_owned();

    let sku = send(
        &app,
        "POST",
        &format!("/v1/admin/products/{pid}/skus"),
        Some(&admin),
        Some(json!({
            "code": "BK-RED-M", "name": "Red / M", "options": {"colour": "red", "size": "M"},
            "stock_available": 5, "weight_g": 450,
            "prices": [{"currency": "MYR", "amount": 12900, "compare_at_amount": 15900},
                       {"currency": "USD", "amount": 2900}]
        })),
    )
    .await;
    assert_eq!(sku.status, StatusCode::CREATED, "{:?}", sku.json);

    // Drafts are invisible to shoppers.
    let list = send(&app, "GET", "/v1/products?currency=MYR", None, None).await;
    assert!(names(&list.json).is_empty());
    assert_eq!(
        send(&app, "GET", "/v1/products/baju-kurung-moden-red", None, None)
            .await
            .status,
        StatusCode::NOT_FOUND
    );

    let patched = send(
        &app,
        "PATCH",
        &format!("/v1/admin/products/{pid}"),
        Some(&admin),
        Some(json!({"status": "active"})),
    )
    .await;
    assert_eq!(patched.json["status"], "active");

    let list = send(
        &app,
        "GET",
        "/v1/products?currency=MYR&category=baju-kurung",
        None,
        None,
    )
    .await;
    let item = &list.json["items"][0];
    assert_eq!(item["price_from"], json!({"amount": 12900, "currency": "MYR"}));
    assert_eq!(item["compare_at"], json!({"amount": 15900, "currency": "MYR"}));
    assert_eq!(item["in_stock"], true);

    // Default currency comes from config (USD in tests).
    let page = send(&app, "GET", "/v1/products/baju-kurung-moden-red", None, None).await;
    assert_eq!(page.status, StatusCode::OK);
    assert_eq!(
        page.json["variants"][0]["price"],
        json!({"amount": 2900, "currency": "USD"})
    );
    assert_eq!(page.json["categories"][0]["slug"], "baju-kurung");
    assert!(
        page.json["variants"][0].get("stock_available").is_none(),
        "exact stock stays private"
    );

    // No JPY price: not sold in JPY, so not listed and no product page.
    assert!(names(&send(&app, "GET", "/v1/products?currency=JPY", None, None).await.json).is_empty());
    assert_eq!(
        send(
            &app,
            "GET",
            "/v1/products/baju-kurung-moden-red?currency=JPY",
            None,
            None
        )
        .await
        .status,
        StatusCode::NOT_FOUND
    );
    assert_eq!(
        send(&app, "GET", "/v1/products?currency=XYZ", None, None).await.status,
        StatusCode::BAD_REQUEST
    );
}

#[sqlx::test]
async fn search_sort_and_paging(db: PgPool) {
    let app = common::router(db.clone(), common::config());
    let admin = admin_token(&app, &db).await;
    product(&app, &admin, "Tudung Bawal Satin", 3500, 3).await;
    product(&app, &admin, "Kuih Lapis Gift Box", 4800, 0).await;
    product(&app, &admin, "Baju Kurung Moden", 12900, 2).await;

    let q = |query: &str| format!("/v1/products?currency=MYR&{query}");
    assert_eq!(
        names(&send(&app, "GET", &q("q=kurung"), None, None).await.json),
        ["Baju Kurung Moden"]
    );
    assert_eq!(
        names(&send(&app, "GET", &q("q=gift%20box"), None, None).await.json),
        ["Kuih Lapis Gift Box"]
    );
    // Typo-tolerant.
    assert_eq!(
        names(&send(&app, "GET", &q("q=tudng"), None, None).await.json),
        ["Tudung Bawal Satin"]
    );
    // LIKE wildcards in the query are literal, not "match everything".
    assert!(names(&send(&app, "GET", &q("q=%25"), None, None).await.json).is_empty());

    assert_eq!(
        names(&send(&app, "GET", &q("sort=price_asc"), None, None).await.json),
        ["Tudung Bawal Satin", "Kuih Lapis Gift Box", "Baju Kurung Moden"]
    );
    assert_eq!(
        names(&send(&app, "GET", &q("sort=price_desc"), None, None).await.json),
        ["Baju Kurung Moden", "Kuih Lapis Gift Box", "Tudung Bawal Satin"]
    );

    let p1 = send(&app, "GET", &q("sort=price_asc&per_page=2"), None, None).await;
    assert_eq!(names(&p1.json).len(), 2);
    assert_eq!(p1.json["has_more"], true);
    let p2 = send(&app, "GET", &q("sort=price_asc&per_page=2&page=2"), None, None).await;
    assert_eq!(names(&p2.json), ["Baju Kurung Moden"]);
    assert_eq!(p2.json["has_more"], false);

    let lapis = send(&app, "GET", &q("q=lapis"), None, None).await;
    assert_eq!(lapis.json["items"][0]["in_stock"], false);
}

#[sqlx::test]
async fn stock_only_moves_by_relative_adjustments(db: PgPool) {
    let app = common::router(db.clone(), common::config());
    let admin = admin_token(&app, &db).await;
    let (_, sku) = product(&app, &admin, "Kuih Lapis", 4800, 5).await;
    let stock = format!("/v1/admin/skus/{sku}/stock");

    let res = send(&app, "POST", &stock, Some(&admin), Some(json!({"delta": -10}))).await;
    assert_eq!(res.status, StatusCode::CONFLICT);
    assert_eq!(
        send(&app, "POST", &stock, Some(&admin), Some(json!({"delta": 5})))
            .await
            .json["stock_available"],
        10
    );

    // 20 concurrent "-1"s against 10 units: exactly 10 succeed, never below zero.
    let tasks: Vec<_> = (0..20)
        .map(|_| {
            let (app, admin, stock) = (app.clone(), admin.clone(), stock.clone());
            tokio::spawn(async move {
                send(&app, "POST", &stock, Some(&admin), Some(json!({"delta": -1})))
                    .await
                    .status
            })
        })
        .collect();
    let mut ok = 0;
    for t in tasks {
        if t.await.unwrap() == StatusCode::OK {
            ok += 1;
        }
    }
    assert_eq!(ok, 10);
    let level: i32 = sqlx::query_scalar("SELECT stock_available FROM skus")
        .fetch_one(&db)
        .await
        .unwrap();
    assert_eq!(level, 0);
}

fn png(w: u32, h: u32) -> Vec<u8> {
    let img = image::DynamicImage::ImageRgb8(image::RgbImage::from_pixel(w, h, image::Rgb([180, 20, 60])));
    let mut out = std::io::Cursor::new(Vec::new());
    img.write_to(&mut out, image::ImageFormat::Png).unwrap();
    out.into_inner()
}

#[sqlx::test]
async fn images_are_processed_stored_served_and_cleaned_up(db: PgPool) {
    let app = common::router(db.clone(), common::config());
    let admin = admin_token(&app, &db).await;
    let (pid, _) = product(&app, &admin, "Tudung Bawal", 3500, 1).await;
    let upload = format!("/v1/admin/products/{pid}/images?alt=Front%20view");

    let res = send_bytes(&app, &upload, &admin, "image/png", png(2400, 1200)).await;
    assert_eq!(res.status, StatusCode::CREATED, "{:?}", res.json);
    assert_eq!(res.json["alt"], "Front view");
    let large = res.json["urls"]["large"].as_str().unwrap().to_owned();
    let thumb = res.json["urls"]["thumb"].as_str().unwrap().to_owned();
    assert!(large.starts_with("/media/images/") && large.ends_with("/large.webp"));

    let served = send(&app, "GET", &large, None, None).await;
    assert_eq!(served.status, StatusCode::OK);
    assert_eq!(served.headers["content-type"], "image/webp");
    assert!(served.headers["cache-control"].to_str().unwrap().contains("immutable"));

    // The storefront list uses the first image as the thumbnail.
    let list = send(&app, "GET", "/v1/products?currency=MYR", None, None).await;
    assert_eq!(list.json["items"][0]["thumbnail"], thumb.as_str());

    // Rejected by content, whatever the client claims.
    let fake = send_bytes(&app, &upload, &admin, "image/png", b"<?php echo 1; ?>".to_vec()).await;
    assert_eq!(fake.status, StatusCode::UNSUPPORTED_MEDIA_TYPE);

    let image_id = res.json["id"].as_str().unwrap();
    assert_eq!(
        send(
            &app,
            "DELETE",
            &format!("/v1/admin/images/{image_id}"),
            Some(&admin),
            None
        )
        .await
        .status,
        StatusCode::NO_CONTENT
    );
    assert_eq!(
        send(&app, "GET", &large, None, None).await.status,
        StatusCode::NOT_FOUND
    );
}

#[sqlx::test]
async fn shared_image_files_survive_until_last_use(db: PgPool) {
    let app = common::router(db.clone(), common::config());
    let admin = admin_token(&app, &db).await;
    let (a, _) = product(&app, &admin, "Product A", 100, 1).await;
    let (b, _) = product(&app, &admin, "Product B", 100, 1).await;
    let bytes = png(300, 300);
    let img_a = send_bytes(
        &app,
        &format!("/v1/admin/products/{a}/images"),
        &admin,
        "image/png",
        bytes.clone(),
    )
    .await;
    let img_b = send_bytes(
        &app,
        &format!("/v1/admin/products/{b}/images"),
        &admin,
        "image/png",
        bytes,
    )
    .await;
    let url = img_a.json["urls"]["large"].as_str().unwrap().to_owned();
    assert_eq!(
        url,
        img_b.json["urls"]["large"].as_str().unwrap(),
        "same picture, stored once"
    );

    send(
        &app,
        "DELETE",
        &format!("/v1/admin/images/{}", img_a.json["id"].as_str().unwrap()),
        Some(&admin),
        None,
    )
    .await;
    assert_eq!(
        send(&app, "GET", &url, None, None).await.status,
        StatusCode::OK,
        "B still uses it"
    );
    send(
        &app,
        "DELETE",
        &format!("/v1/admin/images/{}", img_b.json["id"].as_str().unwrap()),
        Some(&admin),
        None,
    )
    .await;
    assert_eq!(send(&app, "GET", &url, None, None).await.status, StatusCode::NOT_FOUND);
}

#[sqlx::test]
async fn validation_and_permissions(db: PgPool) {
    let app = common::router(db.clone(), common::config());
    let admin = admin_token(&app, &db).await;
    let customer = send(
        &app,
        "POST",
        "/v1/auth/register",
        None,
        Some(json!({"email": "c@shop.test", "password": "correct horse battery"})),
    )
    .await;
    let customer = customer.json["token"].as_str().unwrap().to_owned();

    assert_eq!(
        send(
            &app,
            "POST",
            "/v1/admin/products",
            Some(&customer),
            Some(json!({"name": "x"}))
        )
        .await
        .status,
        StatusCode::FORBIDDEN
    );
    assert_eq!(
        send(&app, "GET", "/v1/admin/products", None, None).await.status,
        StatusCode::UNAUTHORIZED
    );

    let (pid, _) = product(&app, &admin, "Kuih Lapis", 4800, 1).await;
    let dup = send(
        &app,
        "POST",
        "/v1/admin/products",
        Some(&admin),
        Some(json!({"name": "Kuih Lapis"})),
    )
    .await;
    assert_eq!(dup.status, StatusCode::CONFLICT);
    let bad_slug = send(
        &app,
        "POST",
        "/v1/admin/products",
        Some(&admin),
        Some(json!({"name": "X", "slug": "Not A Slug"})),
    )
    .await;
    assert_eq!(bad_slug.status, StatusCode::BAD_REQUEST);

    let two_myr = send(
        &app,
        "POST",
        &format!("/v1/admin/products/{pid}/skus"),
        Some(&admin),
        Some(json!({
            "code": "DUP-CUR", "prices": [{"currency": "MYR", "amount": 1}, {"currency": "MYR", "amount": 2}]
        })),
    )
    .await;
    assert_eq!(two_myr.status, StatusCode::BAD_REQUEST);
    let neg = send(
        &app,
        "POST",
        &format!("/v1/admin/products/{pid}/skus"),
        Some(&admin),
        Some(json!({
            "code": "NEG", "prices": [{"currency": "MYR", "amount": -1}]
        })),
    )
    .await;
    assert_eq!(neg.status, StatusCode::BAD_REQUEST);

    // Categories can't become their own ancestors.
    let parent = send(
        &app,
        "POST",
        "/v1/admin/categories",
        Some(&admin),
        Some(json!({"name": "Clothing"})),
    )
    .await;
    let parent_id = parent.json["id"].as_str().unwrap();
    let child = send(
        &app,
        "POST",
        "/v1/admin/categories",
        Some(&admin),
        Some(json!({"name": "Tudung", "parent_id": parent_id})),
    )
    .await;
    let child_id = child.json["id"].as_str().unwrap();
    let cycle = send(
        &app,
        "PATCH",
        &format!("/v1/admin/categories/{parent_id}"),
        Some(&admin),
        Some(json!({"parent_id": child_id})),
    )
    .await;
    assert_eq!(cycle.status, StatusCode::BAD_REQUEST);
    // `null` moves a category to the top level; omitting the field leaves it alone.
    let moved = send(
        &app,
        "PATCH",
        &format!("/v1/admin/categories/{child_id}"),
        Some(&admin),
        Some(json!({"name": "Tudung & Shawl"})),
    )
    .await;
    assert_eq!(moved.json["parent_id"], parent_id);
    let top = send(
        &app,
        "PATCH",
        &format!("/v1/admin/categories/{child_id}"),
        Some(&admin),
        Some(json!({"parent_id": null})),
    )
    .await;
    assert_eq!(top.json["parent_id"], Value::Null);
}

/// A PNG of random noise, which barely compresses: about `w * h * 3` bytes.
fn noisy_png(w: u32, h: u32) -> Vec<u8> {
    let mut seed: u32 = 0x9e37_79b9;
    let img = image::RgbImage::from_fn(w, h, |_, _| {
        seed = seed.wrapping_mul(1_664_525).wrapping_add(1_013_904_223);
        let [a, b, c, _] = seed.to_le_bytes();
        image::Rgb([a, b, c])
    });
    let mut out = std::io::Cursor::new(Vec::new());
    image::DynamicImage::ImageRgb8(img)
        .write_to(&mut out, image::ImageFormat::Png)
        .unwrap();
    out.into_inner()
}

#[sqlx::test]
async fn phone_sized_photos_upload_up_to_the_configured_limit(db: PgPool) {
    let app = common::router(db.clone(), common::config()); // body_limit_bytes = 10 MiB
    let admin = admin_token(&app, &db).await;
    let (pid, _) = product(&app, &admin, "Tudung Bawal", 3500, 1).await;
    let upload = format!("/v1/admin/products/{pid}/images");

    let photo = noisy_png(1200, 1000);
    assert!(
        photo.len() > 3_000_000,
        "test photo should be bigger than axum's 2 MB default: {}",
        photo.len()
    );
    let res = send_bytes(&app, &upload, &admin, "image/png", photo).await;
    assert_eq!(res.status, StatusCode::CREATED, "{:?}", res.json);

    let too_big = vec![0u8; 11 * 1024 * 1024];
    let res = send_bytes(&app, &upload, &admin, "image/png", too_big).await;
    assert_eq!(res.status, StatusCode::PAYLOAD_TOO_LARGE);
}
