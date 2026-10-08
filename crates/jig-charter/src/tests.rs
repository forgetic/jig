use alloc::boxed::Box;
use skein_lib::{Duration, Reader};
use smith_charter::v1 as wire;

use crate::{
    Budget, Charter, Contract, EndpointName, Model, Prices, Section, TextRule, Tool, ToolEffect, charter, encode,
};

#[test]
fn core_charter_encodes_with_the_same_sections_tools_contract_and_model() {
    let model = Model {
        prices: Prices { input: 2, cached: 1, output: 3, unit: 1000 },
        dialect: 4,
        account: 5,
        endpoint: 7,
        name: Box::from(&b"small"[..]),
        max_tokens: 128,
    };
    let source = Charter {
        instructions: Box::from(&b"help"[..]),
        tools: Box::new([Tool {
            name: Box::from(&b"read"[..]),
            description: Box::from(&b"Read a value"[..]),
            schema: Box::from(&b"{}"[..]),
            effect: ToolEffect::Read,
            timeout: Duration::ZERO,
        }]),
        wait: true,
        agents: false,
        workspace: crate::WorkspaceTools { inspect: false, modify: false, shell: false },
        conventions: None,
        contract: Contract {
            report: Some(TextRule { max: 512, fields: Box::new([]) }),
            failure: None,
            verdicts: Box::new([]),
            change: None,
        },
        budget: Budget { turns: 3, spend: 9000, time: Duration::ZERO },
        model,
        models: Box::new([]),
        waiting: Duration::ZERO,
        resumes: true,
    };
    let sections = Box::new([Section { title: Box::from(&b"Task"[..]), text: Box::from(&b"Do it"[..]) }]);
    let charter = charter(source, sections, false).expect("bounded charter");
    let names = [EndpointName { number: 7, dialect: 4, account: 5, name: Box::from(&b"primary"[..]) }];
    let bytes = encode(charter, &names, &wire::CEILINGS).expect("bounded encoding");
    let decoded = wire::Charter::decode(&wire::CEILINGS, &mut Reader::new(&bytes)).expect("Smith codec accepts it");
    assert_eq!(decoded.instructions(), b"help");
    assert_eq!(decoded.brief().get(0).expect("one section").title(), b"Task");
    assert_eq!(decoded.tools().host().get(0).expect("one tool").name(), b"read");
    assert_eq!(decoded.main().endpoint(), b"primary");
    assert_eq!(decoded.budget().spend(), 9000);
    assert!(!decoded.resume(), "a resume requires a validated transcript");
}
