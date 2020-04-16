// Besides the schema, Diesel also requires us to write a model. This we have to write ourselves, in src/models.rs
use schema::messages;

#[derive(Queryable, Serialize, Debug)]
pub struct Message {
    pub id: i32,
    pub username: String,
    pub message: String,
    pub timestamp: i64,
}

// Diesel can directly associate the fields of our struct with the columns in the database
// The table must be called "messages", as indicated by the table_name attribute.
#[derive(Insertable, Debug)]
#[table_name = "messages"]
pub struct NewMessage {
    pub username: String,
    pub message: String,
}
