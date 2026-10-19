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

#[test]
fn extracts_memoized_and_declared_react_components() {
    let javascript_source = r#"
const TransactionRow = React.memo(({ row, selected }) => {
    const isSelected = selected === row.id;
    return isSelected;
});

export default function ReceiptPaymentListPage({ fixedType }) {
    const navigate = useNavigate();
    const [loading, setLoading] = useState(false);
    const fetchTransactions = async () => {
        const response = await getTransactions();
        return response;
    };
}
"#;

    let outline = extract_outline(javascript_source, "ReceiptPaymentListPage.js", Some("javascript"));
    assert_eq!(outline.symbols.len(), 2);

    let transaction_row = &outline.symbols[0];
    assert_eq!(transaction_row.name, "TransactionRow");
    assert_eq!(transaction_row.children[0].name, "row");
    assert_eq!(transaction_row.children[1].name, "selected");
    assert_eq!(transaction_row.children[2].name, "isSelected");

    let page = &outline.symbols[1];
    assert_eq!(page.name, "ReceiptPaymentListPage");
    assert_eq!(page.visibility.as_deref(), Some("export"));
    assert_eq!(page.children[0].name, "fixedType");
    assert_eq!(page.children[1].name, "navigate");
    assert_eq!(page.children[2].name, "loading");
    assert_eq!(page.children[3].name, "setLoading");
    assert_eq!(page.children[4].name, "fetchTransactions");
    assert_eq!(page.children[4].children[0].name, "response");
}

#[test]
fn extracts_tsx_component_and_enum_outline() {
    let tsx_source = r#"
export enum OpportunityStage {
    Draft = "draft",
    Active = "active",
}

type OpportunityInfoTabProps = {
    opportunityId: string;
};

const NestedButton = React.memo(
    React.forwardRef(({ label }, ref) => {
        const handleClick = () => {};
        return <button ref={ref} onClick={handleClick}>{label}</button>;
    })
);

export default function OpportunityInfoTab({ opportunityId }: OpportunityInfoTabProps) {
    const [previewOpen, setPreviewOpen] = useState(false);
    const handleSelectUnit = (val: string) => {
        return val;
    };
    return (
        <div>
            <NestedButton label="Save" />
        </div>
    );
}
"#;

    // Test with language = None (auto-detected as "tsx" via path)
    let outline_auto = extract_outline(tsx_source, "OpportunityInfoTab.tsx", None);
    assert_eq!(outline_auto.language.as_deref(), Some("tsx"));
    assert_eq!(outline_auto.symbols.len(), 4);

    let stage_enum = &outline_auto.symbols[0];
    assert_eq!(stage_enum.name, "OpportunityStage");
    assert_eq!(stage_enum.kind, SymbolKind::Enum);
    assert_eq!(stage_enum.children.len(), 2);
    assert_eq!(stage_enum.children[0].name, "Draft");
    assert_eq!(stage_enum.children[1].name, "Active");

    let props_type = &outline_auto.symbols[1];
    assert_eq!(props_type.name, "OpportunityInfoTabProps");
    assert_eq!(props_type.kind, SymbolKind::TypeAlias);

    let nested_button = &outline_auto.symbols[2];
    assert_eq!(nested_button.name, "NestedButton");
    assert_eq!(nested_button.kind, SymbolKind::Function);
    assert_eq!(nested_button.children[0].name, "label");
    assert_eq!(nested_button.children[1].name, "handleClick");

    let comp = &outline_auto.symbols[3];
    assert_eq!(comp.name, "OpportunityInfoTab");
    assert_eq!(comp.kind, SymbolKind::Function);
    assert_eq!(comp.visibility.as_deref(), Some("export"));
    assert_eq!(comp.children[0].name, "opportunityId");
    assert_eq!(comp.children[1].name, "previewOpen");
    assert_eq!(comp.children[2].name, "setPreviewOpen");
    assert_eq!(comp.children[3].name, "handleSelectUnit");

    // Test with explicit language = "typescript" to verify automatic TSX fallback
    let outline_ts_fallback = extract_outline(tsx_source, "OpportunityInfoTab.tsx", Some("typescript"));
    assert_eq!(outline_ts_fallback.symbols.len(), 4);
    assert_eq!(outline_ts_fallback.symbols[3].name, "OpportunityInfoTab");
}

