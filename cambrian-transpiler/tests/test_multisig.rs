// Copyright (C) 2025-2026 The Cambrian Authors
// SPDX-License-Identifier: GPL-3.0-only

use cambrian_transpiler::ProgramParser;

#[test]
fn parse_multisig_cam() {
    let src = include_str!("../../contracts/multisig.cam");
    let program = ProgramParser::new().parse(src).unwrap();

    // Pure functions
    assert_eq!(program.pure_fns.len(), 12);
    assert_eq!(program.pure_fns[0].name, "get_mask_value");
    assert_eq!(program.pure_fns[11].name, "remove_expired_transactions");

    // Entity
    assert_eq!(program.entities.len(), 1);
    let entity = &program.entities[0];
    assert_eq!(entity.name, "MultisigWallet");

    // Records
    assert_eq!(entity.records.len(), 2);
    assert_eq!(entity.records[0].name, "Transaction");
    assert_eq!(entity.records[0].fields.len(), 12);
    assert_eq!(entity.records[1].name, "CustodianInfo");

    // Constants
    assert_eq!(entity.constants.len(), 16);

    // Macros
    assert_eq!(entity.macros.len(), 3);
    assert_eq!(entity.macros[0].name, "cleaned_transactions");
    assert_eq!(entity.macros[1].name, "custodian_not_confirmed");
    assert_eq!(entity.macros[2].name, "transaction_will_execute");

    // Routes
    assert_eq!(entity.routes.len(), 17);

    // State members
    assert_eq!(entity.members.len(), 8);
    assert_eq!(entity.members[0].name, "m_owner_key");
    assert_eq!(entity.members[7].name, "m_max_cleanup_txns");
}
