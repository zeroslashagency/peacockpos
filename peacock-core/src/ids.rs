//! Newtype IDs.
//!
//! Frappe uses bare strings for every Link field, which makes it trivial to pass
//! a room name where a table name belongs. These newtypes make that a compile error.

use serde::{Deserialize, Serialize};
use std::fmt;

macro_rules! id_newtype {
    ($(#[$meta:meta])* $name:ident) => {
        $(#[$meta])*
        #[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
        pub struct $name(pub String);

        impl $name {
            pub fn new(s: impl Into<String>) -> Self {
                $name(s.into())
            }
            pub fn as_str(&self) -> &str {
                &self.0
            }
        }

        impl fmt::Display for $name {
            fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
                write!(f, "{}", self.0)
            }
        }

        impl From<&str> for $name {
            fn from(s: &str) -> Self {
                $name(s.to_owned())
            }
        }
    };
}

// URY-owned entities
id_newtype!(
    /// `URY Table`
    TableName
);
id_newtype!(
    /// `URY Room`
    RoomName
);
id_newtype!(
    /// `URY Restaurant`
    RestaurantName
);
id_newtype!(
    /// `URY Production Unit`
    ProductionUnitName
);
id_newtype!(
    /// `URY Menu`
    MenuName
);
id_newtype!(
    /// `URY Menu Course`
    MenuCourseName
);
id_newtype!(
    /// `URY KOT`
    KotName
);

// ERPNext-owned entities. URY only holds Links to these — it does not own the tables.
id_newtype!(
    /// ERPNext `Item`
    ItemCode
);
id_newtype!(
    /// ERPNext `Item Group` — drives KOT station routing
    ItemGroupName
);
id_newtype!(
    /// ERPNext `Customer`
    CustomerName
);
id_newtype!(
    /// ERPNext `Branch`
    BranchName
);
id_newtype!(
    /// ERPNext `POS Profile`
    PosProfileName
);
id_newtype!(
    /// ERPNext `POS Invoice` — the actual order of record
    InvoiceName
);
id_newtype!(
    /// ERPNext `Price List`
    PriceListName
);
id_newtype!(
    /// ERPNext `BOM`
    BomName
);
id_newtype!(
    /// Frappe `User`
    UserName
);
