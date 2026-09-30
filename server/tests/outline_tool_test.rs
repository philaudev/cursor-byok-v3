use serde_json::json;

#[tokio::test]
async fn test_outline_on_company_service() {
    let company_service_path = "D:/Admin/Documents/PROJECTS/conruabien/sea-turtle-crm-service/src/main/java/com/crb/crm/service/impl/CompanyService.java";
    
    let path_to_test = if std::path::Path::new(company_service_path).exists() {
        company_service_path.to_string()
    } else {
        format!("{}/src/lib.rs", env!("CARGO_MANIFEST_DIR"))
    };

    let args = json!({
        "path": path_to_test,
    });

    let result = cursor_server::search::execute_outline(args).await;
    assert!(result.is_ok(), "Outline execution should succeed: {:?}", result.err());

    let val = result.unwrap();
    assert_eq!(val["file_path"], path_to_test);
    assert!(val["total_lines"].as_u64().unwrap() > 0);
    assert!(val["rendered"].as_str().unwrap().contains("Outline for"));
    assert!(val["symbols"].as_array().unwrap().len() > 0);
}

#[tokio::test]
async fn test_outline_on_python_file() {
    let py_path = "D:/Admin/Documents/PROJECTS/vnrex_crm/vnrex_crm_be/apps/opportunities/services.py";
    if !std::path::Path::new(py_path).exists() {
        return;
    }

    let args = json!({
        "path": py_path,
    });

    let result = cursor_server::search::execute_outline(args).await;
    assert!(result.is_ok(), "Python outline execution should succeed: {:?}", result.err());

    let val = result.unwrap();
    println!("--- Python Outline for services.py ---\n{}", val["rendered"].as_str().unwrap());
}

#[tokio::test]
async fn test_outline_on_typescript_file() {
    let ts_path = "D:/Admin/Documents/PROJECTS/vnrex_crm/vnrex_crm_fe/src/shared/utils/project.ts";
    if !std::path::Path::new(ts_path).exists() {
        return;
    }

    let args = json!({
        "path": ts_path,
    });

    let result = cursor_server::search::execute_outline(args).await;
    assert!(result.is_ok(), "TS outline execution should succeed: {:?}", result.err());

    let val = result.unwrap();
    println!("--- TypeScript Outline for project.ts ---\n{}", val["rendered"].as_str().unwrap());
}

#[tokio::test]
async fn test_outline_on_tsx_file() {
    let tsx_path = "D:/Admin/Documents/PROJECTS/vnrex_crm/vnrex_crm_fe/src/pages/opportunities/components/OpportunityKanbanBoard.tsx";
    if !std::path::Path::new(tsx_path).exists() {
        return;
    }

    let args = json!({
        "path": tsx_path,
    });

    let result = cursor_server::search::execute_outline(args).await;
    assert!(result.is_ok(), "TSX outline execution should succeed: {:?}", result.err());

    let val = result.unwrap();
    println!("--- React TSX Outline for OpportunityKanbanBoard.tsx ---\n{}", val["rendered"].as_str().unwrap());
}
