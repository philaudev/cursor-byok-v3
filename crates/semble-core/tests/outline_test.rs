use semble_core::{extract_outline, SymbolKind};

#[test]
fn extracts_java_class_outline() {
    let java_source = r#"
package com.crb.crm.service.impl;

@Service
public class CompanyService implements ICompanyService {
    private final ICompanyRepository companyRepository;
    private final SecurityUtility securityUtility;

    public CompanyService(ICompanyRepository repo, SecurityUtility sec) {
        this.companyRepository = repo;
        this.securityUtility = sec;
    }

    @Override
    public ResponsePayload getNames() {
        return null;
    }

    @Override
    public ResponsePayload getAll(Pageable pageable) {
        return null;
    }
}
"#;

    let outline = extract_outline(java_source, "CompanyService.java", Some("java"));
    assert_eq!(outline.file_path, "CompanyService.java");
    assert_eq!(outline.language.as_deref(), Some("java"));
    assert_eq!(outline.symbols.len(), 1);

    let cls = &outline.symbols[0];
    assert_eq!(cls.name, "CompanyService");
    assert_eq!(cls.kind, SymbolKind::Class);
    assert_eq!(cls.visibility.as_deref(), Some("public"));
    assert_eq!(cls.children.len(), 5);

    // fields
    assert_eq!(cls.children[0].name, "companyRepository");
    assert_eq!(cls.children[0].kind, SymbolKind::Field);
    assert_eq!(cls.children[0].detail.as_deref(), Some(": ICompanyRepository"));

    // constructor
    assert_eq!(cls.children[2].name, "CompanyService");
    assert_eq!(cls.children[2].kind, SymbolKind::Constructor);

    // methods
    assert_eq!(cls.children[3].name, "getNames");
    assert_eq!(cls.children[3].kind, SymbolKind::Method);
    assert_eq!(cls.children[3].detail.as_deref(), Some("(): ResponsePayload"));

    let ascii = outline.render_ascii_tree();
    assert!(ascii.contains("CompanyService"));
    assert!(ascii.contains("companyRepository"));
    assert!(ascii.contains("getNames(): ResponsePayload"));
}

#[test]
fn extracts_rust_outline() {
    let rust_source = r#"
pub struct Config {
    pub name: String,
    port: u16,
}

impl Config {
    pub fn new(name: String) -> Self {
        Self { name, port: 8080 }
    }
}
"#;

    let outline = extract_outline(rust_source, "config.rs", Some("rust"));
    assert_eq!(outline.symbols.len(), 2);
    assert_eq!(outline.symbols[0].name, "Config");
    assert_eq!(outline.symbols[0].kind, SymbolKind::Struct);
    assert_eq!(outline.symbols[0].children.len(), 2);
    assert_eq!(outline.symbols[1].name, "impl Config");
}
