use logzz::domain::parser::Parser; // поправь путь под свой крейт

#[test]
fn repro_your_samples() {
    let parser = Parser::new();

    let samples = [
        "https://ems.nu.ac.bd/student-login:19219255849:123456",
        "https://pandauth.com/register:anhtaizman:FPkyfKiYcbcgFX3",
        "http://192.168.130.200/:admin:admin",
        "https://mega.nz/:yessira75@hotmail.com:Vensurosa75",
        "https://www.netflix.com/id-en/login:juliolopez@belidulu.me:mineral",
        "https://partner.steamgames.com/:david_clp:#Maracuja01",
        "https://accounts.lidl.com/account/register:967516805:*W18cajosapo18Y#",
        "https://elamigosedition.com/index.php:Lucas Emanuel Batista Fernandes:LsjxzDCdN.d2X65",
        "android://rx1M7HNclOtlTbka8IqBM_RA-PPzS-htzHtHBJQcOM2oNQSOsZxZF1j1MPteckITXrJ7xKh6mEC2HmKZpBwBrg==@com.snapchat.android/:uchidamsgame:julie19/05",
        "android://kMwTAtZmFfKmIQbZNR-R5WGLVpMFvwE1vjtIBnOYw6edB4Xe-A-Y1sNGSx48BKZIQZX1xXWSqtyT32Fzdjfb9g==@com.futbin/:henriquebassalo:jcchaos007",
    ];

    let input = samples.join("\n");
    let report = parser.parse_text(&input, "repro.txt");

    println!("records = {}, issues = {}", report.records.len(), report.issues.len());

    for (i, r) in report.records.iter().enumerate() {
        println!(
            "[{i}] url={:?}\n    user={:?}\n    pass={:?}",
            r.url, r.username, r.password
        );
    }
    for (i, iss) in report.issues.iter().enumerate() {
        println!("[issue {i}] {} | lines={:?}", iss.message, iss.raw_lines);
    }

    assert_eq!(report.records.len(), samples.len(), "не все записи распознаны");
    assert!(report.issues.is_empty(), "есть issues");
}
