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

#[test]
fn extracts_javascript_react_component_outline() {
    let javascript_source = r#"
export const PaymentVoucher = ({ propId, isModal, onClose }) => {
    const [isLoading, setIsLoading] = useState(false);
    const initialFormData = {
        type: "payment",
        partnerId: propId,
    };
    const handleReset = () => setIsLoading(false);
    useEffect(() => {
        const interval = setInterval(handleReset, 1000);
        return () => clearInterval(interval);
    }, []);
};

const LegacyVoucher = function ({ readOnly }) {
    const visible = !readOnly;
    return visible;
};
"#;

    let outline = extract_outline(javascript_source, "PaymentVoucher.js", Some("javascript"));
    assert_eq!(outline.symbols.len(), 2);

    let payment_voucher = &outline.symbols[0];
    assert_eq!(payment_voucher.name, "PaymentVoucher");
    assert_eq!(payment_voucher.kind, SymbolKind::Function);
    assert_eq!(payment_voucher.visibility.as_deref(), Some("export"));
    assert_eq!(payment_voucher.detail.as_deref(), Some("({ propId, isModal, onClose })"));
    assert_eq!(payment_voucher.children[0].name, "propId");
    assert_eq!(payment_voucher.children[3].name, "isLoading");
    assert_eq!(payment_voucher.children[3].detail.as_deref(), Some(" state from useState(false)"));
    assert_eq!(payment_voucher.children[4].name, "setIsLoading");
    assert_eq!(payment_voucher.children[4].detail.as_deref(), Some(" state setter"));
    assert_eq!(payment_voucher.children[5].name, "initialFormData");
    assert_eq!(payment_voucher.children[5].children[0].name, "type");
    assert_eq!(payment_voucher.children[5].children[1].name, "partnerId");
    assert_eq!(payment_voucher.children[6].name, "handleReset");
    assert_eq!(payment_voucher.children[6].kind, SymbolKind::Function);
    assert_eq!(payment_voucher.children[7].name, "useEffect");
    assert_eq!(payment_voucher.children[7].children[0].name, "interval");

    let legacy_voucher = &outline.symbols[1];
    assert_eq!(legacy_voucher.name, "LegacyVoucher");
    assert_eq!(legacy_voucher.kind, SymbolKind::Function);
    assert_eq!(legacy_voucher.children[0].name, "readOnly");
    assert_eq!(legacy_voucher.children[1].name, "visible");
}
