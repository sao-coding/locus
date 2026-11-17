# Writes deterministic synthetic fixtures (no real data):
#   <OutDir>\fixtures\student.tsv       17 columns x 40,450 rows  -> bench.student      (sidecar tests)
#   <OutDir>\fixtures-real\student.tsv  23 columns x 40,450 rows  -> bench.student_real (db-mysql, locus-core tests)
# The second file has the same header, row count and kinds of text (CJK names, empty cells, repeated
# userId) as the masked real file the tests are normally run with, but every value is made up.
# The same seed always gives the same bytes. Usage: pwsh scripts/ci/gen-fixtures.ps1 [-OutDir devenv]
param([string]$OutDir = (Join-Path (Split-Path (Split-Path $PSScriptRoot)) 'devenv'))
$ErrorActionPreference = 'Stop'

Add-Type -Language CSharp -TypeDefinition @'
using System;
using System.IO;
using System.Text;

public static class FixtureGen
{
    const int Rows = 40450;

    static string Cjk(Random r, int n)
    {
        var sb = new StringBuilder();
        for (int i = 0; i < n; i++) sb.Append((char)(0x4E00 + r.Next(0, 0x9FA5 - 0x4E00)));
        return sb.ToString();
    }

    static string Digits(Random r, int n)
    {
        var sb = new StringBuilder();
        for (int i = 0; i < n; i++) sb.Append((char)('0' + r.Next(10)));
        return sb.ToString();
    }

    static string Letters(Random r, int n)
    {
        var sb = new StringBuilder();
        for (int i = 0; i < n; i++) sb.Append((char)('a' + r.Next(26)));
        return sb.ToString();
    }

    static readonly string[] Words = { "course", "select", "review", "credit", "transfer", "approved", "tuition", "scholarship", "grade", "student", "update", "pending", "remark" };
    static readonly string[] Depts = { "Finance", "Business", "Computer Science", "Law", "Nursing", "Design", "Physics", "History" };

    static string Phrase(Random r, int n)
    {
        var sb = new StringBuilder();
        for (int i = 0; i < n; i++) { if (i > 0) sb.Append(' '); sb.Append(Words[r.Next(Words.Length)]); }
        return sb.ToString();
    }

    public static void Student(string path)
    {
        var r = new Random(20261123);
        var utf8 = new UTF8Encoding(false);
        using (var w = new StreamWriter(path, false, utf8, 1 << 16))
        {
            w.NewLine = "\n";
            w.Write("id\tstudent_no\tname\tgender\tbirthday\temail\tphone\tclass_id\tdepartment\tenroll_year\tgpa\tcredits\taddress\tnote\tprofile\tcreated_at\tupdated_at\n");
            var epoch = new DateTime(2000, 1, 1);
            for (int id = 1; id <= Rows; id++)
            {
                int year = 2018 + r.Next(0, 8);
                var created = new DateTime(year, 1, 1).AddSeconds(r.Next(0, 300 * 24 * 3600)).AddMilliseconds(r.Next(0, 1000));
                var updated = new DateTime(created.Year, created.Month, created.Day, created.Hour, created.Minute, created.Second);
                var line = new StringBuilder();
                line.Append(id).Append('\t');
                line.Append("S").Append(year).Append(id.ToString("D5")).Append('\t');
                line.Append(Cjk(r, 3)).Append('\t');
                line.Append(r.Next(2) == 0 ? "M" : "F").Append('\t');
                line.Append(epoch.AddDays(r.Next(0, 3000)).ToString("yyyy-MM-dd")).Append('\t');
                line.Append('u').Append(id).Append("@example.edu\t");
                line.Append("09").Append(Digits(r, 8)).Append('\t');
                line.Append(100 + r.Next(0, 300)).Append('\t');
                line.Append(Depts[r.Next(Depts.Length)]).Append('\t');
                line.Append(year).Append('\t');
                line.Append((r.Next(0, 400) / 100.0).ToString("0.00", System.Globalization.CultureInfo.InvariantCulture)).Append('\t');
                line.Append(r.Next(0, 140)).Append('\t');
                line.Append(Cjk(r, 3)).Append("市").Append(Cjk(r, 2)).Append("區").Append(r.Next(1, 500)).Append("號\t");
                line.Append(Phrase(r, r.Next(2, 8))).Append('\t');
                line.Append("{\"club\":\"").Append(Words[r.Next(Words.Length)]).Append("\",\"tags\":[\"").Append(Words[r.Next(Words.Length)]).Append("\",\"").Append(Words[r.Next(Words.Length)]).Append("\"],\"score\":").Append(r.Next(0, 100)).Append("}\t");
                line.Append(created.ToString("yyyy-MM-dd HH:mm:ss.fff")).Append('\t');
                line.Append(updated.ToString("yyyy-MM-dd HH:mm:ss"));
                w.Write(line.ToString()); w.Write('\n');
            }
        }
    }

    public static void StudentReal(string path)
    {
        var r = new Random(20261124);
        var utf8 = new UTF8Encoding(false);
        using (var w = new StreamWriter(path, false, utf8, 1 << 16))
        {
            w.NewLine = "\n";
            w.Write("userId\tpassword\tlineId\temail\timg\tname\tenName\tIDNumber\tstatus\tmajor\tgender\tbirthday\tbirthplace\taddress\tphone\thomePhone\tdate\tpreviousEducation\tparentName\tparentAddress\tparentPhone\tparentJob\tdesc\n");
            for (int i = 0; i < Rows; i++)
            {
                // About 2.7 rows per userId, like the masked file (userId is not unique there).
                string userId = "M" + (75000000 + (long)i * 15224 / Rows).ToString();
                var c = new string[23];
                c[0] = userId;
                c[1] = "";
                c[2] = "";
                c[3] = "E" + Digits(r, 8) + "@" + Letters(r, 6) + ".example.test";
                c[4] = "https://img.example.test/" + Letters(r, 8) + "/" + userId + ".png";
                c[5] = Cjk(r, 3);
                c[6] = Letters(r, 2).ToUpperInvariant() + ", " + Letters(r, 3).ToUpperInvariant() + "-" + Letters(r, 5).ToUpperInvariant();
                c[7] = "C" + Digits(r, 9);
                c[8] = Cjk(r, 2);
                c[9] = Cjk(r, 5);
                c[10] = Cjk(r, 1);
                c[11] = Digits(r, 7);
                c[12] = Cjk(r, 3);
                c[13] = Cjk(r, 3) + Digits(r, 3) + Cjk(r, 2) + Digits(r, 2) + Cjk(r, 2);
                c[14] = Digits(r, 10);
                c[15] = "(" + Digits(r, 2) + ")" + Digits(r, 8);
                c[16] = Digits(r, 3) + " " + Cjk(r, 1) + " " + r.Next(1, 4) + " " + Cjk(r, 1);
                c[17] = Cjk(r, 20);
                c[18] = Cjk(r, 3);
                c[19] = Cjk(r, 8) + Digits(r, 2) + Cjk(r, 2);
                c[20] = Digits(r, 10);
                c[21] = Cjk(r, 6) + Digits(r, 3) + Cjk(r, 3) + Digits(r, 2);
                c[22] = (i % 7 == 0) ? Cjk(r, 12) : "";
                w.Write(string.Join("\t", c)); w.Write('\n');
            }
        }
    }
}
'@

$a = Join-Path $OutDir 'fixtures'
$b = Join-Path $OutDir 'fixtures-real'
New-Item -ItemType Directory -Force $a, $b | Out-Null
[FixtureGen]::Student((Join-Path (Resolve-Path $a) 'student.tsv'))
[FixtureGen]::StudentReal((Join-Path (Resolve-Path $b) 'student.tsv'))
"wrote $a\student.tsv and $b\student.tsv"
