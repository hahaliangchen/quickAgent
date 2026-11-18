// Customer-only first-run initializer. This is compiled as a separate command
// into the selected ai-ppt-server snapshot; it never imports a database dump.
package main

import (
    "encoding/json"
    "fmt"
    "os"
    "path/filepath"

    "github.com/flipped-aurora/gin-vue-admin/server/core"
    "github.com/flipped-aurora/gin-vue-admin/server/global"
    model "github.com/flipped-aurora/gin-vue-admin/server/model/system"
    "github.com/flipped-aurora/gin-vue-admin/server/model/system/request"
    service "github.com/flipped-aurora/gin-vue-admin/server/service/system"
    _ "github.com/flipped-aurora/gin-vue-admin/server/source/system"
    "github.com/flipped-aurora/gin-vue-admin/server/utils"
    adapter "github.com/casbin/gorm-adapter/v3"
    "gorm.io/driver/mysql"
    "gorm.io/gorm"
)

func main() {
    if err := initialize(); err != nil { fmt.Fprintln(os.Stderr, "customer initialization:", err); os.Exit(1) }
}

func initialize() error {
    marker := "/state/customer-initialized.json"
    if _, err := os.Stat(marker); err == nil { fmt.Println("Customer database already initialized."); return nil }
    password := os.Getenv("CUSTOMER_ADMIN_PASSWORD")
    if len(password) < 12 { return fmt.Errorf("CUSTOMER_ADMIN_PASSWORD must contain at least 12 characters") }
    global.GVA_VP = core.Viper()
    global.GVA_LOG = core.Zap()
    cfg := global.GVA_CONFIG.Mysql
    db, err := gorm.Open(mysql.Open(cfg.Dsn()), &gorm.Config{DisableForeignKeyConstraintWhenMigrating:true})
    if err != nil { return err }
    var tables int64
    if err := db.Raw("SELECT COUNT(*) FROM information_schema.tables WHERE table_schema = ?", cfg.Dbname).Scan(&tables).Error; err != nil { return err }
    if tables != 0 { return fmt.Errorf("database is not empty; refusing first-run initialization") }
    init := service.InitDBService{}
    if err := init.InitDB(request.InitDB{DBType:"mysql", Host:cfg.Path, Port:cfg.Port,
        UserName:cfg.Username, Password:cfg.Password, DBName:cfg.Dbname, AdminPassword:utils.PasswordMD5(password)}); err != nil { return err }
    db = global.GVA_DB
    if err := db.Transaction(func(tx *gorm.DB) error {
        if err := tx.Unscoped().Where("username <> ?", "admin").Delete(&model.SysUser{}).Error; err != nil { return err }
        var admin model.SysUser
        if err := tx.Where("username = ?", "admin").First(&admin).Error; err != nil { return err }
        if err := tx.Model(&admin).Updates(map[string]any{"nick_name":"客户管理员", "phone":"", "email":"", "header_img":"", "authority_id":888, "enable":1}).Error; err != nil { return err }
        var role model.SysAuthority
        if err := tx.Where("authority_id = ?",888).First(&role).Error; err != nil { return err }
        if err := tx.Model(&admin).Association("Authorities").Replace([]model.SysAuthority{role}); err != nil { return err }
        if err := tx.Exec("DELETE FROM sys_authority_menus").Error; err != nil { return err }
        if err := tx.Unscoped().Where("1=1").Delete(&model.SysBaseMenu{}).Error; err != nil { return err }
        pptRoot := model.SysBaseMenu{ParentId:0, Path:"ppt-manage", Name:"pptManageRoot", Component:"#", Sort:1, Meta:model.Meta{Title:"PPT管理", Icon:"hub"}}
        wordRoot := model.SysBaseMenu{ParentId:0, Path:"word-manage", Name:"wordManage", Component:"#", Sort:2, Meta:model.Meta{Title:"WORD管理", Icon:"description"}}
        modelRoot := model.SysBaseMenu{ParentId:0, Path:"modelCenter", Name:"modelCenter", Component:"view/modelCenter/index.vue", Sort:3, Meta:model.Meta{Title:"模型中心", Icon:"model_training"}}
        roots := []*model.SysBaseMenu{&pptRoot, &wordRoot, &modelRoot}
        for _, root := range roots {
            if err := tx.Create(root).Error; err != nil { return err }
        }
        children := []model.SysBaseMenu{
            {ParentId:pptRoot.ID, MenuLevel:1, Path:"create-ppt", Name:"createPpt", Component:"react/createPpt", Sort:1, Meta:model.Meta{Title:"创建PPT", Icon:"post_add"}},
            {ParentId:pptRoot.ID, MenuLevel:1, Path:"ppt-history", Name:"pptHistory", Component:"react/pptHistory", Sort:2, Meta:model.Meta{Title:"历史会话", Icon:"history"}},
            {ParentId:wordRoot.ID, MenuLevel:1, Path:"document", Name:"documentManage", Component:"view/example/document/document.vue", Sort:1, Meta:model.Meta{Title:"文档管理", Icon:"folder"}},
            {ParentId:wordRoot.ID, MenuLevel:1, Path:"onlyoffice-plugin", Name:"onlyofficePlugin", Component:"view/example/onlyofficePlugin/onlyofficePlugin.vue", Sort:2, Meta:model.Meta{Title:"插件管理", Icon:"extension"}},
            {ParentId:modelRoot.ID, MenuLevel:1, Path:"modelAccess", Name:"modelAccess", Component:"view/modelCenter/modelAccess/index.vue", Sort:1, Meta:model.Meta{Title:"模型接入", Icon:"api"}},
        }
        if err := tx.Create(&children).Error; err != nil { return err }
        allMenus := append([]model.SysBaseMenu{pptRoot, wordRoot, modelRoot}, children...)
        if err := tx.Model(&role).Association("SysBaseMenus").Replace(allMenus); err != nil { return err }
        if err := tx.Model(&role).Update("default_router","createPpt").Error; err != nil { return err }
        // Grant newly introduced PPT, documentManage, and modelAccess APIs for role 888.
        for _, rule := range []struct{ path, method string }{
            {"/aiPpt/plugin/api/plugin/*", "POST"},
            {"/aiPpt/plugin/deck/svg", "POST"},
            {"/aiPpt/plugin/templateDraft/preview", "POST"},
            {"/aiPpt/plugin/template/render", "GET"},
            {"/documentManage/getDocumentDetail/:id", "GET"},
            {"/documentManage/deleteDocumentCategory/:id", "DELETE"},
            {"/documentManage/updateDocumentPermissions", "PUT"},
            {"/documentManage/updateDocumentPermissions", "POST"},
            {"/modelAccess/getModelVendorList", "POST"},
            {"/modelAccess/createModelVendor", "POST"},
            {"/modelAccess/updateModelVendor", "PUT"},
            {"/modelAccess/deleteModelVendor", "DELETE"},
            {"/modelAccess/findModelVendor", "GET"},
            {"/modelAccess/updateModelVendorStatus", "PUT"},
            {"/modelAccess/testModelVendorConnection", "POST"},
            {"/modelAccess/getModelConfigList", "POST"},
            {"/modelAccess/createModelConfig", "POST"},
            {"/modelAccess/updateModelConfig", "PUT"},
            {"/modelAccess/deleteModelConfig", "DELETE"},
            {"/modelAccess/findModelConfig", "GET"},
            {"/modelAccess/updateModelConfigStatus", "PUT"},
            {"/modelAccess/setVendorDefaultModel", "PUT"},
            {"/modelAccess/setCategoryDefaultModel", "PUT"},
            {"/modelAccess/testModelConfig", "POST"},
            {"/modelAccess/invokeModel", "POST"},
            {"/modelCategory/getModelCategoryOptions", "GET"},
            {"/modelCategory/getModelCategoryTree", "GET"},
        } {
            policy := adapter.CasbinRule{Ptype:"p", V0:"888", V1:rule.path, V2:rule.method}
            if err := tx.Where("ptype = ? AND v0 = ? AND v1 = ? AND v2 = ?", policy.Ptype, policy.V0, policy.V1, policy.V2).FirstOrCreate(&policy).Error; err != nil { return err }
        }
        if err := tx.Unscoped().Where("1=1").Delete(&model.SysAIModel{}).Error; err != nil { return err }
        if err := tx.Unscoped().Where("1=1").Delete(&model.SysAIProvider{}).Error; err != nil { return err }
        return nil
    }); err != nil { return err }
    if err := os.MkdirAll(filepath.Dir(marker),0700); err != nil { return err }
    data,_ := json.MarshalIndent(map[string]any{"menus":[]string{"create-ppt","ppt-history","document","onlyoffice-plugin","modelAccess"},"providers":[]string{},"models":[]string{},"admin":"admin"},"","  ")
    if err := os.WriteFile(marker,append(data,'\n'),0600); err != nil { return err }
    fmt.Println("Customer database initialized; PPT, documentManage, onlyofficePlugin and modelAccess menus enabled, no model providers or models.")
    return nil
}
